//! The Linux `vhci_hcd` virtual host controller: attached devices show up on
//! it as if plugged into a local USB port.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use usbridge_usbip::{Speed, UsbDevice};

const VDEV_ST_NOTASSIGNED: u32 = 5;
const VDEV_ST_USED: u32 = 6;
const VDEV_ST_NULL: u32 = 4;
const EBUSY: i32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hub {
    High,
    Super,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub hub: Hub,
    pub port: u32,
    pub status: u32,
    pub devid: u32,
    pub local_busid: String,
}

impl Port {
    pub fn is_free(&self) -> bool {
        self.status == VDEV_ST_NULL
    }

    /// Still carrying the device we attached, or about to.
    pub fn holds(&self, devid: u32) -> bool {
        self.status == VDEV_ST_NOTASSIGNED || (self.status == VDEV_ST_USED && self.devid == devid)
    }
}

pub struct Vhci {
    dir: PathBuf,
}

impl Default for Vhci {
    fn default() -> Self {
        Self::new("/sys/devices/platform/vhci_hcd.0")
    }
}

impl Vhci {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn ensure_loaded(&self) -> Result<()> {
        if self.dir.is_dir() {
            return Ok(());
        }
        let _ = Command::new("modprobe").arg("vhci-hcd").status();
        if !self.dir.is_dir() {
            bail!(
                "the vhci-hcd kernel driver is not available. Install your distro's kernel \
                 extra modules (Debian/Ubuntu: linux-modules-extra-$(uname -r)) and check \
                 `modprobe vhci-hcd`"
            );
        }
        Ok(())
    }

    /// All ports across every vhci controller (`status`, `status.1`, ...).
    pub fn ports(&self) -> Result<Vec<Port>> {
        let mut out = Vec::new();
        for i in 0.. {
            let name = if i == 0 {
                "status".to_owned()
            } else {
                format!("status.{i}")
            };
            match fs::read_to_string(self.dir.join(&name)) {
                Ok(text) => out.extend(parse_status(&text)),
                Err(e) if i > 0 && e.kind() == io::ErrorKind::NotFound => break,
                Err(e) => return Err(e).context(format!("reading vhci {name}")),
            }
        }
        Ok(out)
    }

    /// Plug the device into a free port, handing the kernel the socket it was
    /// imported on. Returns the port number.
    pub fn attach(&self, dev: &UsbDevice, fd: RawFd) -> Result<u32> {
        let hub = if dev.speed.is_superspeed() {
            Hub::Super
        } else {
            Hub::High
        };
        // vhci_hcd rejects SUPER_PLUS; the device still runs on the SS hub.
        let speed = match dev.speed {
            Speed::SuperPlus => Speed::Super,
            s => s,
        };
        let free: Vec<u32> = self
            .ports()?
            .into_iter()
            .filter(|p| p.hub == hub && p.is_free())
            .map(|p| p.port)
            .collect();
        if free.is_empty() {
            bail!("no free vhci {hub:?}-speed port; detach something or raise the port count");
        }
        for port in free {
            let line = format!("{port} {fd} {} {}", dev.devid(), speed.as_u32());
            match write_once(&self.dir.join("attach"), &line) {
                Ok(()) => return Ok(port),
                // Another attach took this port in the meantime.
                Err(e) if e.raw_os_error() == Some(EBUSY) => continue,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    bail!("permission denied writing to vhci_hcd; run as root")
                }
                Err(e) => return Err(e).context("vhci_hcd refused the attach"),
            }
        }
        bail!("every free vhci port was taken while attaching")
    }

    pub fn detach(&self, port: u32) -> Result<()> {
        write_once(&self.dir.join("detach"), &port.to_string())
            .with_context(|| format!("detaching vhci port {port}"))
    }
}

fn write_once(path: &std::path::Path, value: &str) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(path)?
        .write_all(value.as_bytes())
}

/// Parses the `status` attribute (Linux 4.13+ format):
///
/// ```text
/// hub port sta spd dev      sockfd local_busid
/// hs  0000 006 002 00010005 000003 3-1
/// ss  0008 004 000 00000000 000000 0-0
/// ```
pub fn parse_status(text: &str) -> Vec<Port> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [hub, port, sta, _spd, dev, _sockfd, busid] = f[..] else {
                return None;
            };
            Some(Port {
                hub: match hub {
                    "hs" => Hub::High,
                    "ss" => Hub::Super,
                    _ => return None,
                },
                port: port.parse().ok()?,
                status: sta.parse().ok()?,
                devid: u32::from_str_radix(dev, 16).ok()?,
                local_busid: busid.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
hub port sta spd dev      sockfd local_busid
hs  0000 006 002 00010005 000003 3-1
hs  0001 004 000 00000000 000000 0-0
ss  0008 004 000 00000000 000000 0-0
";

    fn device(speed: Speed) -> UsbDevice {
        UsbDevice {
            path: String::new(),
            busid: "zigbee".into(),
            busnum: 1,
            devnum: 5,
            speed,
            vendor_id: 0x10c4,
            product_id: 0xea60,
            bcd_device: 0,
            class: 0,
            subclass: 0,
            protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 1,
        }
    }

    #[test]
    fn parses_status() {
        let ports = parse_status(STATUS);
        assert_eq!(ports.len(), 3);
        assert_eq!(ports[0].port, 0);
        assert!(ports[0].holds(0x0001_0005));
        assert!(!ports[0].holds(0x0001_0006));
        assert!(ports[1].is_free());
        assert_eq!(ports[2].hub, Hub::Super);
        assert_eq!(ports[2].port, 8);
    }

    fn fake() -> (tempfile::TempDir, Vhci) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("status"), STATUS).unwrap();
        fs::write(dir.path().join("attach"), "").unwrap();
        let v = Vhci::new(dir.path());
        (dir, v)
    }

    #[test]
    fn attach_picks_free_port_on_matching_hub() {
        let (dir, v) = fake();
        assert_eq!(v.attach(&device(Speed::Full), 9).unwrap(), 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("attach")).unwrap(),
            "1 9 65541 2"
        );

        assert_eq!(v.attach(&device(Speed::SuperPlus), 9).unwrap(), 8);
        assert_eq!(
            fs::read_to_string(dir.path().join("attach")).unwrap(),
            "8 9 65541 5"
        );
    }

    #[test]
    fn reads_additional_controllers() {
        let (dir, v) = fake();
        fs::write(
            dir.path().join("status.1"),
            "hub port sta spd dev      sockfd local_busid\nhs  0016 004 000 00000000 000000 0-0\n",
        )
        .unwrap();
        assert_eq!(v.ports().unwrap().len(), 4);
    }
}
