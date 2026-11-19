//! Byte layouts of the usbip-win2 VHCI driver's IOCTLs (include/usbip/vhci.h).
//!
//! Byte layouts match usbip-win2 v0.9.8.1, the version pinned by
//! install/install-client.ps1. Kept platform-neutral so it is tested on
//! every OS.
#![cfg_attr(not(windows), allow(dead_code))]

use anyhow::{bail, Result};

/// {B4030C06-DC5F-4FCC-87EB-E5515A0935C0}
pub const VHCI_INTERFACE_GUID: u128 = 0xB4030C06_DC5F_4FCC_87EB_E5515A0935C0;

const fn ctl_code(function: u32) -> u32 {
    const FILE_DEVICE_UNKNOWN: u32 = 0x22;
    const METHOD_BUFFERED: u32 = 0;
    const FILE_READ_WRITE_DATA: u32 = 0x1 | 0x2;
    (FILE_DEVICE_UNKNOWN << 16) | (FILE_READ_WRITE_DATA << 14) | (function << 2) | METHOD_BUFFERED
}

pub const IOCTL_PLUGOUT_HARDWARE: u32 = ctl_code(0x801);
pub const IOCTL_GET_IMPORTED_DEVICES: u32 = ctl_code(0x802);
/// Like PLUGIN_HARDWARE, but the driver does not retry in the background;
/// reconnecting is our job, the same as on Linux.
pub const IOCTL_PLUGIN_HARDWARE_ONCE: u32 = ctl_code(0x806);

const BUSID: (usize, usize) = (0, 32);
const SERVICE: (usize, usize) = (32, 32);
const HOST: (usize, usize) = (64, 1025);
const LOCATION_LEN: usize = 32 + 32 + 1025;

/// `struct plugin_hardware : base, imported_device_location {
/// char serial[16]; bool wsk_events; }`, padded to 4.
pub const PLUGIN_LEN: usize = 1124;
const PLUGIN_LOCATION: usize = 12;
const PLUGIN_SERIAL: (usize, usize) = (1104, 16);
const PLUGIN_WSK_EVENTS: usize = 1120;
/// The driver writes back `size` and `port` only.
pub const PLUGIN_REPLY_LEN: usize = 8;

pub const PLUGOUT_LEN: usize = 8;

pub const IMPORTED_DEVICE_LEN: usize = 1132;
const IMPORTED_LOCATION: usize = 8;
const DEVICES_OFFSET: usize = 4;
/// `sizeof(get_imported_devices)`, which has a one-element array.
const GET_IMPORTED_SIZE: u32 = (DEVICES_OFFSET + IMPORTED_DEVICE_LEN) as u32;

const _: () = assert!(PLUGIN_LOCATION + LOCATION_LEN == 1101);
const _: () = assert!(PLUGIN_SERIAL.0 == 1104 && PLUGIN_SERIAL.1 == 16);
const _: () = assert!(PLUGIN_WSK_EVENTS == 1120);
const _: () = assert!(PLUGIN_LOCATION + LOCATION_LEN + 16 < PLUGIN_LEN);
const _: () = assert!(IMPORTED_LOCATION + LOCATION_LEN <= IMPORTED_DEVICE_LEN);

pub fn plugin_request(host: &str, port: u16, busid: &str) -> Result<Vec<u8>> {
    let mut b = vec![0u8; PLUGIN_LEN];
    b[..4].copy_from_slice(&(PLUGIN_LEN as u32).to_le_bytes());
    let loc = &mut b[PLUGIN_LOCATION..PLUGIN_LOCATION + LOCATION_LEN];
    put(loc, BUSID, busid, "device id")?;
    put(loc, SERVICE, &port.to_string(), "port")?;
    put(loc, HOST, host, "host name")?;
    // serial (empty) and wsk_events (false = zero-copy, the default) stay zero.
    Ok(b)
}

pub fn plugin_reply_port(reply: &[u8]) -> Result<u32> {
    if reply.len() != PLUGIN_REPLY_LEN {
        bail!(
            "usbip-win2 driver gave a {}-byte reply, expected {PLUGIN_REPLY_LEN}",
            reply.len()
        );
    }
    let port = i32::from_le_bytes(reply[4..8].try_into().unwrap());
    if port <= 0 {
        bail!("usbip-win2 driver returned port {port}");
    }
    Ok(port as u32)
}

pub fn plugout_request(port: u32) -> [u8; PLUGOUT_LEN] {
    let mut b = [0u8; PLUGOUT_LEN];
    b[..4].copy_from_slice(&(PLUGOUT_LEN as u32).to_le_bytes());
    b[4..].copy_from_slice(&(port as i32).to_le_bytes());
    b
}

/// A buffer for GET_IMPORTED_DEVICES with room for `n` devices.
pub fn imported_request(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; DEVICES_OFFSET + n * IMPORTED_DEVICE_LEN];
    b[..4].copy_from_slice(&GET_IMPORTED_SIZE.to_le_bytes());
    b
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub port: u32,
    pub busid: String,
    pub service: String,
    pub host: String,
}

pub fn parse_imported(reply: &[u8]) -> Result<Vec<Imported>> {
    if reply.len() < DEVICES_OFFSET
        || !(reply.len() - DEVICES_OFFSET).is_multiple_of(IMPORTED_DEVICE_LEN)
    {
        bail!(
            "usbip-win2 driver gave a malformed device list ({} bytes)",
            reply.len()
        );
    }
    Ok(reply[DEVICES_OFFSET..]
        .as_chunks::<IMPORTED_DEVICE_LEN>()
        .0
        .iter()
        .map(|d| {
            let loc = &d[IMPORTED_LOCATION..IMPORTED_LOCATION + LOCATION_LEN];
            Imported {
                port: i32::from_le_bytes(d[..4].try_into().unwrap()).max(0) as u32,
                busid: get(loc, BUSID),
                service: get(loc, SERVICE),
                host: get(loc, HOST),
            }
        })
        .collect())
}

fn put(buf: &mut [u8], (off, len): (usize, usize), s: &str, what: &str) -> Result<()> {
    if s.len() >= len {
        bail!("{what} {s:?} is longer than {} bytes", len - 1);
    }
    buf[off..off + s.len()].copy_from_slice(s.as_bytes());
    Ok(())
}

fn get(buf: &[u8], (off, len): (usize, usize)) -> String {
    let field = &buf[off..off + len];
    let end = field.iter().position(|&c| c == 0).unwrap_or(len);
    String::from_utf8_lossy(&field[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_codes_match_ctl_code_macro() {
        assert_eq!(IOCTL_PLUGOUT_HARDWARE, 0x0022_E004);
        assert_eq!(IOCTL_GET_IMPORTED_DEVICES, 0x0022_E008);
        assert_eq!(IOCTL_PLUGIN_HARDWARE_ONCE, 0x0022_E018);
    }

    #[test]
    fn plugin_request_layout() {
        let b = plugin_request("usbgw.local", 3240, "zigbee").unwrap();
        assert_eq!(b.len(), 1124);
        assert_eq!(&b[..4], &1124u32.to_le_bytes());
        assert_eq!(&b[12..18], b"zigbee");
        assert_eq!(&b[44..48], b"3240");
        assert_eq!(&b[76..87], b"usbgw.local");
        assert_eq!(PLUGIN_SERIAL.0, 1104);
        assert_eq!(PLUGIN_WSK_EVENTS, 1120);
        assert!(b[87..PLUGIN_SERIAL.0].iter().all(|&c| c == 0));
        assert!(b[PLUGIN_SERIAL.0..PLUGIN_LEN].iter().all(|&c| c == 0));
        assert!(plugin_request("h", 1, &"x".repeat(32)).is_err());
    }

    #[test]
    fn plugin_reply() {
        let mut r = [0u8; 8];
        r[4..].copy_from_slice(&3i32.to_le_bytes());
        assert_eq!(plugin_reply_port(&r).unwrap(), 3);
        assert!(plugin_reply_port(&[0u8; 8]).is_err());
        assert!(plugin_reply_port(&[0u8; 4]).is_err());
    }

    #[test]
    fn plugout_request_layout() {
        assert_eq!(plugout_request(5), [8, 0, 0, 0, 5, 0, 0, 0]);
    }

    #[test]
    fn imported_roundtrip() {
        let req = imported_request(2);
        assert_eq!(&req[..4], &1136u32.to_le_bytes());

        let mut reply = vec![0u8; 4 + 2 * IMPORTED_DEVICE_LEN];
        for (i, (port, busid)) in [(1i32, "zigbee"), (4, "printer")].iter().enumerate() {
            let d = &mut reply[4 + i * IMPORTED_DEVICE_LEN..];
            d[..4].copy_from_slice(&port.to_le_bytes());
            d[8..8 + busid.len()].copy_from_slice(busid.as_bytes());
            d[40..44].copy_from_slice(b"3240");
            d[72..75].copy_from_slice(b"gw1");
        }
        let got = parse_imported(&reply).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[1],
            Imported {
                port: 4,
                busid: "printer".into(),
                service: "3240".into(),
                host: "gw1".into(),
            }
        );
        assert!(parse_imported(&reply[..100]).is_err());
        assert!(parse_imported(&reply[..4]).unwrap().is_empty());
    }
}
