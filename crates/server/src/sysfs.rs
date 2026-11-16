//! Reading USB devices out of sysfs and driving the `usbip-host` driver.
//!
//! The root is configurable so tests can run against a fake tree.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use farplug_usbip::{ExportedDevice, Speed, UsbDevice, UsbInterface, PATH_LEN};

pub const USBIP_HOST: &str = "usbip-host";
const HUB_CLASS: u8 = 0x09;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysDevice {
    pub busid: String,
    pub path: PathBuf,
    pub busnum: u32,
    pub devnum: u32,
    pub speed: Speed,
    pub vendor_id: u16,
    pub product_id: u16,
    pub bcd_device: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub configuration_value: u8,
    pub num_configurations: u8,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub driver: Option<String>,
    pub interfaces: Vec<UsbInterface>,
}

impl SysDevice {
    pub fn is_hub(&self) -> bool {
        self.class == HUB_CLASS
    }

    pub fn is_bound_to_usbip(&self) -> bool {
        self.driver.as_deref() == Some(USBIP_HOST)
    }

    pub fn description(&self) -> String {
        match (&self.manufacturer, &self.product) {
            (Some(m), Some(p)) => format!("{m} {p}"),
            (None, Some(p)) => p.clone(),
            (Some(m), None) => m.clone(),
            (None, None) => String::new(),
        }
    }

    /// `busid` is the name the client asked for, which `usbip attach` requires
    /// to be echoed back verbatim.
    pub fn to_usbip(&self, busid: &str) -> UsbDevice {
        let path = self.path.to_string_lossy();
        UsbDevice {
            path: if path.len() < PATH_LEN {
                path.into_owned()
            } else {
                String::new()
            },
            busid: busid.to_owned(),
            busnum: self.busnum,
            devnum: self.devnum,
            speed: self.speed,
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            bcd_device: self.bcd_device,
            class: self.class,
            subclass: self.subclass,
            protocol: self.protocol,
            configuration_value: self.configuration_value,
            num_configurations: self.num_configurations,
            num_interfaces: self.interfaces.len() as u8,
        }
    }

    pub fn exported(&self, busid: &str) -> ExportedDevice {
        ExportedDevice {
            device: self.to_usbip(busid),
            interfaces: self.interfaces.clone(),
        }
    }
}

/// `usbip_status` of a device bound to `usbip-host`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StubStatus {
    Available,
    Used,
    Error,
}

#[derive(Debug, Clone)]
pub struct Sysfs {
    root: PathBuf,
}

impl Sysfs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn devices_dir(&self) -> PathBuf {
        self.root.join("bus/usb/devices")
    }

    fn stub_dir(&self) -> PathBuf {
        self.root.join("bus/usb/drivers").join(USBIP_HOST)
    }

    pub fn ensure_usbip_host(&self) -> Result<()> {
        if self.stub_dir().is_dir() {
            return Ok(());
        }
        let _ = Command::new("modprobe").arg(USBIP_HOST).status();
        if !self.stub_dir().is_dir() {
            bail!(
                "the {USBIP_HOST} kernel driver is not available. Install your distro's \
                 kernel extra modules (Debian/Ubuntu: linux-modules-extra-$(uname -r) or \
                 linux-image-*; Raspberry Pi OS ships it) and check `modprobe {USBIP_HOST}`"
            );
        }
        Ok(())
    }

    /// Every USB device except root hubs, sorted by busid.
    pub fn scan(&self) -> Result<Vec<SysDevice>> {
        let dir = self.devices_dir();
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.contains(':') || name.starts_with("usb") {
                continue;
            }
            // A device can vanish between readdir and reading its attributes.
            if let Ok(dev) = self.read_device(&name) {
                out.push(dev);
            }
        }
        out.sort_by(|a, b| a.busid.cmp(&b.busid));
        Ok(out)
    }

    pub fn read_device(&self, busid: &str) -> Result<SysDevice> {
        let dir = self.devices_dir().join(busid);
        let attr = |name: &str| read_attr(&dir, name);
        let hex16 = |name: &str| -> Result<u16> {
            let v = attr(name)?;
            u16::from_str_radix(&v, 16).with_context(|| format!("{busid}/{name}: {v:?}"))
        };
        let hex8 = |name: &str| -> Result<u8> {
            let v = attr(name)?;
            u8::from_str_radix(&v, 16).with_context(|| format!("{busid}/{name}: {v:?}"))
        };
        let dec = |name: &str| -> Result<u32> {
            let v = attr(name)?;
            // bConfigurationValue is empty while the device is unconfigured.
            if v.is_empty() {
                return Ok(0);
            }
            v.parse().with_context(|| format!("{busid}/{name}: {v:?}"))
        };

        let configuration_value = dec("bConfigurationValue")? as u8;
        let num_interfaces = dec("bNumInterfaces").unwrap_or(0) as u8;
        let mut interfaces = Vec::with_capacity(num_interfaces.into());
        for i in 0..num_interfaces {
            let ifdir = dir.join(format!("{busid}:{configuration_value}.{i}"));
            let h = |name: &str| -> u8 {
                read_attr(&ifdir, name)
                    .ok()
                    .and_then(|v| u8::from_str_radix(&v, 16).ok())
                    .unwrap_or(0)
            };
            interfaces.push(UsbInterface {
                class: h("bInterfaceClass"),
                subclass: h("bInterfaceSubClass"),
                protocol: h("bInterfaceProtocol"),
            });
        }

        Ok(SysDevice {
            busid: busid.to_owned(),
            path: fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone()),
            busnum: dec("busnum")?,
            devnum: dec("devnum")?,
            speed: speed_from_sysfs(&attr("speed").unwrap_or_default()),
            vendor_id: hex16("idVendor")?,
            product_id: hex16("idProduct")?,
            bcd_device: hex16("bcdDevice").unwrap_or(0),
            class: hex8("bDeviceClass").unwrap_or(0),
            subclass: hex8("bDeviceSubClass").unwrap_or(0),
            protocol: hex8("bDeviceProtocol").unwrap_or(0),
            configuration_value,
            num_configurations: dec("bNumConfigurations").unwrap_or(1) as u8,
            serial: attr("serial").ok(),
            manufacturer: attr("manufacturer").ok(),
            product: attr("product").ok(),
            driver: fs::read_link(dir.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            interfaces,
        })
    }

    /// Detach the device from its host driver and hand it to `usbip-host`.
    pub fn bind(&self, dev: &SysDevice) -> Result<()> {
        if dev.is_bound_to_usbip() {
            return Ok(());
        }
        if dev.driver.is_some() {
            let unbind = self.devices_dir().join(&dev.busid).join("driver/unbind");
            write_attr(&unbind, &dev.busid)?;
        }
        self.match_busid("add", &dev.busid)?;
        if let Err(e) = write_attr(&self.stub_dir().join("bind"), &dev.busid) {
            let _ = self.match_busid("del", &dev.busid);
            return Err(e);
        }
        Ok(())
    }

    /// Take the device away from `usbip-host` and let the host drivers probe it
    /// again. Any client using it is disconnected.
    pub fn release(&self, busid: &str) -> Result<()> {
        let dev = self.read_device(busid)?;
        if dev.is_bound_to_usbip() {
            write_attr(&self.stub_dir().join("unbind"), busid)?;
        }
        self.forget(busid);
        let rebind = self.stub_dir().join("rebind");
        if rebind.exists() {
            write_attr(&rebind, busid)
        } else {
            write_attr(&self.root.join("bus/usb/drivers_probe"), busid)
        }
    }

    /// Stop `usbip-host` from claiming whatever gets plugged into this port next.
    pub fn forget(&self, busid: &str) {
        let _ = self.match_busid("del", busid);
    }

    fn match_busid(&self, op: &str, busid: &str) -> Result<()> {
        write_attr(
            &self.stub_dir().join("match_busid"),
            &format!("{op} {busid}"),
        )
    }

    pub fn stub_status(&self, busid: &str) -> Result<StubStatus> {
        let v = read_attr(&self.devices_dir().join(busid), "usbip_status")?;
        Ok(match v.as_str() {
            "1" => StubStatus::Available,
            "2" => StubStatus::Used,
            _ => StubStatus::Error,
        })
    }

    /// Give a connected socket to the kernel, which then serves URBs on it.
    pub fn hand_over_socket(&self, busid: &str, fd: RawFd) -> Result<()> {
        write_attr(
            &self.devices_dir().join(busid).join("usbip_sockfd"),
            &fd.to_string(),
        )
    }
}

fn read_attr(dir: &Path, name: &str) -> Result<String> {
    let p = dir.join(name);
    let v = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    Ok(v.trim().to_owned())
}

fn write_attr(path: &Path, value: &str) -> Result<()> {
    // sysfs attributes take exactly one write(2); no O_CREAT/O_TRUNC.
    let mut f = OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(value.as_bytes())
        .with_context(|| format!("writing {value:?} to {}", path.display()))
}

fn speed_from_sysfs(mbps: &str) -> Speed {
    match mbps {
        "1.5" => Speed::Low,
        "12" => Speed::Full,
        "480" => Speed::High,
        "53.3-480" => Speed::Wireless,
        "5000" => Speed::Super,
        "10000" | "20000" => Speed::SuperPlus,
        _ => Speed::Unknown,
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A sysfs tree with just enough in it for scanning and binding.
    pub struct FakeSys {
        pub dir: tempfile::TempDir,
    }

    pub struct FakeDev<'a> {
        pub busid: &'a str,
        pub vid: &'a str,
        pub pid: &'a str,
        pub serial: Option<&'a str>,
        pub driver: Option<&'a str>,
        pub class: &'a str,
    }

    impl Default for FakeDev<'_> {
        fn default() -> Self {
            Self {
                busid: "1-1",
                vid: "10c4",
                pid: "ea60",
                serial: None,
                driver: Some("usb"),
                class: "00",
            }
        }
    }

    impl FakeSys {
        pub fn new() -> Self {
            let s = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            fs::create_dir_all(s.root().join("bus/usb/devices")).unwrap();
            fs::create_dir_all(s.root().join("bus/usb/devices/usb1")).unwrap();
            fs::create_dir_all(s.root().join("bus/usb/devices/1-0:1.0")).unwrap();
            let stub = s.root().join("bus/usb/drivers").join(USBIP_HOST);
            fs::create_dir_all(&stub).unwrap();
            for f in ["bind", "unbind", "rebind", "match_busid"] {
                fs::write(stub.join(f), "").unwrap();
            }
            s
        }

        pub fn root(&self) -> &Path {
            self.dir.path()
        }

        pub fn sysfs(&self) -> Sysfs {
            Sysfs::new(self.root())
        }

        pub fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.root().join(rel)).unwrap()
        }

        pub fn add(&self, d: FakeDev) {
            let dir = self.root().join("bus/usb/devices").join(d.busid);
            fs::create_dir_all(&dir).unwrap();
            let attrs = [
                ("idVendor", d.vid),
                ("idProduct", d.pid),
                ("bcdDevice", "0100"),
                ("bDeviceClass", d.class),
                ("bDeviceSubClass", "00"),
                ("bDeviceProtocol", "00"),
                ("bConfigurationValue", "1"),
                ("bNumConfigurations", "1"),
                ("bNumInterfaces", " 1"),
                ("busnum", "1"),
                ("devnum", "5"),
                ("speed", "12"),
                ("product", "CP2102N USB to UART"),
                ("usbip_status", "1"),
                ("usbip_sockfd", ""),
            ];
            for (k, v) in attrs {
                fs::write(dir.join(k), format!("{v}\n")).unwrap();
            }
            if let Some(s) = d.serial {
                fs::write(dir.join("serial"), format!("{s}\n")).unwrap();
            }
            let ifdir = dir.join(format!("{}:1.0", d.busid));
            fs::create_dir_all(&ifdir).unwrap();
            fs::write(ifdir.join("bInterfaceClass"), "ff\n").unwrap();
            if let Some(drv) = d.driver {
                self.set_driver(d.busid, drv);
            }
        }

        pub fn set_driver(&self, busid: &str, driver: &str) {
            let drvdir = self.root().join("bus/usb/drivers").join(driver);
            fs::create_dir_all(&drvdir).unwrap();
            if !drvdir.join("unbind").exists() {
                fs::write(drvdir.join("unbind"), "").unwrap();
            }
            let link = self
                .root()
                .join("bus/usb/devices")
                .join(busid)
                .join("driver");
            let _ = fs::remove_file(&link);
            symlink(&drvdir, link).unwrap();
        }

        pub fn remove(&self, busid: &str) {
            fs::remove_dir_all(self.root().join("bus/usb/devices").join(busid)).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    #[test]
    fn scan_skips_root_hubs_and_interfaces() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev {
            serial: Some("0001"),
            ..Default::default()
        });
        let devs = fs_.sysfs().scan().unwrap();
        assert_eq!(devs.len(), 1);
        let d = &devs[0];
        assert_eq!(d.busid, "1-1");
        assert_eq!((d.vendor_id, d.product_id), (0x10c4, 0xea60));
        assert_eq!(d.serial.as_deref(), Some("0001"));
        assert_eq!(d.speed, Speed::Full);
        assert_eq!(d.driver.as_deref(), Some("usb"));
        assert_eq!(d.interfaces.len(), 1);
        assert_eq!(d.interfaces[0].class, 0xff);
        assert_eq!(d.to_usbip("zigbee").devid(), (1 << 16) | 5);
    }

    #[test]
    fn bind_unbinds_host_driver_first() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev::default());
        let sys = fs_.sysfs();
        let dev = sys.read_device("1-1").unwrap();
        sys.bind(&dev).unwrap();
        assert_eq!(fs_.read("bus/usb/drivers/usb/unbind"), "1-1");
        assert_eq!(
            fs_.read("bus/usb/drivers/usbip-host/match_busid"),
            "add 1-1"
        );
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/bind"), "1-1");
    }

    #[test]
    fn release_rebinds_to_host() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev {
            driver: Some(USBIP_HOST),
            ..Default::default()
        });
        fs_.sysfs().release("1-1").unwrap();
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/unbind"), "1-1");
        assert_eq!(
            fs_.read("bus/usb/drivers/usbip-host/match_busid"),
            "del 1-1"
        );
        assert_eq!(fs_.read("bus/usb/drivers/usbip-host/rebind"), "1-1");
    }

    #[test]
    fn stub_status_and_sockfd() {
        let fs_ = FakeSys::new();
        fs_.add(FakeDev::default());
        let sys = fs_.sysfs();
        assert_eq!(sys.stub_status("1-1").unwrap(), StubStatus::Available);
        sys.hand_over_socket("1-1", 7).unwrap();
        assert_eq!(fs_.read("bus/usb/devices/1-1/usbip_sockfd"), "7");
    }
}
