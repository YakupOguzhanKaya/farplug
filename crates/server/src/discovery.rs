use anyhow::Result;
use tokio_serial::{SerialPortType, UsbPortInfo};
use usbridge_proto::DeviceInfo;

use crate::config::DeviceCfg;

#[derive(Debug, Clone)]
pub struct Found {
    pub path: String,
    pub usb: Option<UsbPortInfo>,
}

/// Resolve a configured device to the serial port it currently occupies.
///
/// Matching is re-run on every attach rather than cached, because the OS path
/// (`/dev/ttyUSB0`, `/dev/cu.usbserial-*`) is reassigned on replug and would
/// otherwise go stale while the daemon keeps running.
pub fn resolve(cfg: &DeviceCfg) -> Result<Option<Found>> {
    let want_vid = cfg.vid_u16()?;
    let want_pid = cfg.pid_u16()?;

    // An explicit path is authoritative: enumeration does not list ptys or
    // non-USB ttys, so going through it would report a present device absent.
    if let Some(path) = &cfg.path {
        if !std::fs::metadata(path).is_ok() {
            return Ok(None);
        }
        let usb = enumerate()?.into_iter().find_map(|(name, usb)| {
            (&name == path).then_some(usb).flatten()
        });
        if !usb_matches(usb.as_ref(), want_vid, want_pid, cfg.serial.as_deref()) {
            return Ok(None);
        }
        return Ok(Some(Found {
            path: path.clone(),
            usb,
        }));
    }

    for (path, usb) in enumerate()? {
        let Some(usb) = usb else { continue };
        if usb_matches(Some(&usb), want_vid, want_pid, cfg.serial.as_deref()) {
            return Ok(Some(Found {
                path,
                usb: Some(usb),
            }));
        }
    }
    Ok(None)
}

fn enumerate() -> Result<Vec<(String, Option<UsbPortInfo>)>> {
    Ok(tokio_serial::available_ports()?
        .into_iter()
        .map(|p| {
            let usb = match p.port_type {
                SerialPortType::UsbPort(info) => Some(info),
                _ => None,
            };
            (p.port_name, usb)
        })
        .collect())
}

fn usb_matches(
    usb: Option<&UsbPortInfo>,
    want_vid: Option<u16>,
    want_pid: Option<u16>,
    want_serial: Option<&str>,
) -> bool {
    if want_vid.is_none() && want_pid.is_none() && want_serial.is_none() {
        return true;
    }
    let Some(usb) = usb else { return false };
    if want_vid.is_some_and(|v| v != usb.vid) {
        return false;
    }
    if want_pid.is_some_and(|p| p != usb.pid) {
        return false;
    }
    if let Some(serial) = want_serial {
        if usb.serial_number.as_deref() != Some(serial) {
            return false;
        }
    }
    true
}

pub fn describe(id: &str, cfg: &DeviceCfg, found: Option<&Found>, busy: bool) -> DeviceInfo {
    let usb = found.and_then(|f| f.usb.as_ref());
    DeviceInfo {
        id: id.to_string(),
        label: cfg.label.clone().unwrap_or_else(|| id.to_string()),
        profile: cfg.profile(),
        vid: usb.map(|u| u.vid),
        pid: usb.map(|u| u.pid),
        serial: usb.and_then(|u| u.serial_number.clone()),
        present: found.is_some(),
        busy,
    }
}
