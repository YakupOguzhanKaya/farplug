//! USB/IP operation-phase messages (device list and import), byte-compatible
//! with the Linux `usbip` / `usbipd` tools. All integers are big-endian.
//!
//! Once an import succeeds the same TCP connection carries URB traffic, which on
//! Linux is handled entirely by the kernel (`usbip-host` / `vhci_hcd`).

pub mod urb;

use std::fmt;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::net::TcpStream;
#[cfg(unix)]
use std::time::Duration;

pub const VERSION: u16 = 0x0111;
pub const DEFAULT_PORT: u16 = 3240;

pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 0x0005;
pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 0x0003;
/// farplug's own request; stock USB/IP peers never send it, and older farplug
/// servers close the connection on it.
pub const OP_REQ_INFO: u16 = 0x8f01;
pub const OP_REP_INFO: u16 = 0x0f01;
const MAX_INFO_LEN: u32 = 1 << 20;

pub const BUSID_LEN: usize = 32;
pub const PATH_LEN: usize = 256;
pub const DEVICE_LEN: usize = PATH_LEN + BUSID_LEN + 24;
const HEADER_LEN: usize = 8;
const INTERFACE_LEN: usize = 4;

/// Longest busid that still leaves room for the NUL terminator.
pub const MAX_BUSID: usize = BUSID_LEN - 1;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("peer speaks USB/IP version {0:#06x}, expected {VERSION:#06x}")]
    Version(u16),
    #[error("unexpected USB/IP op code {got:#06x}, expected {expected:#06x}")]
    UnexpectedOp { got: u16, expected: u16 },
    #[error("unknown USB/IP request {0:#06x}")]
    UnknownRequest(u16),
    #[error("{field} is {len} bytes, limit is {max}")]
    TooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
    #[error("{0} is not valid UTF-8")]
    NotUtf8(&'static str),
    #[error("server refused: {0}")]
    Refused(Status),
    #[error("bad info reply: {0}")]
    BadInfo(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    NotAvailable,
    DeviceBusy,
    DeviceError,
    NoDevice,
    Error,
}

impl Status {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Ok,
            1 => Self::NotAvailable,
            2 => Self::DeviceBusy,
            3 => Self::DeviceError,
            4 => Self::NoDevice,
            _ => Self::Error,
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            Self::Ok => 0,
            Self::NotAvailable => 1,
            Self::DeviceBusy => 2,
            Self::DeviceError => 3,
            Self::NoDevice => 4,
            Self::Error => 5,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::NotAvailable => "device not available",
            Self::DeviceBusy => "device is in use by another client",
            Self::DeviceError => "device error",
            Self::NoDevice => "no such device",
            Self::Error => "unexpected error",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Speed {
    Unknown,
    Low,
    Full,
    High,
    Wireless,
    Super,
    SuperPlus,
}

impl Speed {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Low,
            2 => Self::Full,
            3 => Self::High,
            4 => Self::Wireless,
            5 => Self::Super,
            6 => Self::SuperPlus,
            _ => Self::Unknown,
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            Self::Unknown => 0,
            Self::Low => 1,
            Self::Full => 2,
            Self::High => 3,
            Self::Wireless => 4,
            Self::Super => 5,
            Self::SuperPlus => 6,
        }
    }

    pub fn is_superspeed(self) -> bool {
        matches!(self, Self::Super | Self::SuperPlus)
    }
}

impl fmt::Display for Speed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown speed",
            Self::Low => "1.5 Mb/s",
            Self::Full => "12 Mb/s",
            Self::High => "480 Mb/s",
            Self::Wireless => "wireless",
            Self::Super => "5 Gb/s",
            Self::SuperPlus => "10 Gb/s+",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsbDevice {
    pub path: String,
    pub busid: String,
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
    pub num_interfaces: u8,
}

impl UsbDevice {
    /// The value `vhci_hcd` wants as `devid` when attaching.
    pub fn devid(&self) -> u32 {
        (self.busnum << 16) | (self.devnum & 0xffff)
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        put_str(out, &self.path, PATH_LEN, "path")?;
        put_str(out, &self.busid, BUSID_LEN, "busid")?;
        out.extend_from_slice(&self.busnum.to_be_bytes());
        out.extend_from_slice(&self.devnum.to_be_bytes());
        out.extend_from_slice(&self.speed.as_u32().to_be_bytes());
        out.extend_from_slice(&self.vendor_id.to_be_bytes());
        out.extend_from_slice(&self.product_id.to_be_bytes());
        out.extend_from_slice(&self.bcd_device.to_be_bytes());
        out.extend_from_slice(&[
            self.class,
            self.subclass,
            self.protocol,
            self.configuration_value,
            self.num_configurations,
            self.num_interfaces,
        ]);
        Ok(())
    }

    fn decode(b: &[u8; DEVICE_LEN]) -> Result<Self> {
        let u32_at = |o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let u16_at = |o: usize| u16::from_be_bytes(b[o..o + 2].try_into().unwrap());
        let n = PATH_LEN + BUSID_LEN;
        Ok(Self {
            path: get_str(&b[..PATH_LEN], "path")?,
            busid: get_str(&b[PATH_LEN..n], "busid")?,
            busnum: u32_at(n),
            devnum: u32_at(n + 4),
            speed: Speed::from_u32(u32_at(n + 8)),
            vendor_id: u16_at(n + 12),
            product_id: u16_at(n + 14),
            bcd_device: u16_at(n + 16),
            class: b[n + 18],
            subclass: b[n + 19],
            protocol: b[n + 20],
            configuration_value: b[n + 21],
            num_configurations: b[n + 22],
            num_interfaces: b[n + 23],
        })
    }

    fn read_from(r: &mut impl Read) -> Result<Self> {
        let mut b = [0u8; DEVICE_LEN];
        r.read_exact(&mut b)?;
        Self::decode(&b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsbInterface {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

/// One entry of a device-list reply.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedDevice {
    pub device: UsbDevice,
    pub interfaces: Vec<UsbInterface>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    DevList,
    Import { busid: String },
    Info,
}

impl Request {
    pub fn write_to(&self, w: &mut impl Write) -> Result<()> {
        let mut out = Vec::with_capacity(HEADER_LEN + BUSID_LEN);
        match self {
            Self::DevList => put_header(&mut out, OP_REQ_DEVLIST, Status::Ok),
            Self::Info => put_header(&mut out, OP_REQ_INFO, Status::Ok),
            Self::Import { busid } => {
                put_header(&mut out, OP_REQ_IMPORT, Status::Ok);
                put_str(&mut out, busid, BUSID_LEN, "busid")?;
            }
        }
        w.write_all(&out)?;
        Ok(())
    }

    pub fn read_from(r: &mut impl Read) -> Result<Self> {
        let (code, _) = read_header(r)?;
        match code {
            OP_REQ_DEVLIST => Ok(Self::DevList),
            OP_REQ_INFO => Ok(Self::Info),
            OP_REQ_IMPORT => {
                let mut b = [0u8; BUSID_LEN];
                r.read_exact(&mut b)?;
                Ok(Self::Import {
                    busid: get_str(&b, "busid")?,
                })
            }
            other => Err(Error::UnknownRequest(other)),
        }
    }
}

pub fn write_devlist_reply(w: &mut impl Write, devices: &[ExportedDevice]) -> Result<()> {
    let mut out = Vec::with_capacity(HEADER_LEN + 4 + devices.len() * (DEVICE_LEN + 16));
    put_header(&mut out, OP_REP_DEVLIST, Status::Ok);
    out.extend_from_slice(&(devices.len() as u32).to_be_bytes());
    for d in devices {
        d.device.encode(&mut out)?;
        // The reader trusts bNumInterfaces for how many records follow.
        let n = usize::from(d.device.num_interfaces);
        for i in 0..n {
            let ifc = d.interfaces.get(i).copied().unwrap_or(UsbInterface {
                class: 0,
                subclass: 0,
                protocol: 0,
            });
            out.extend_from_slice(&[ifc.class, ifc.subclass, ifc.protocol, 0]);
        }
    }
    w.write_all(&out)?;
    Ok(())
}

pub fn read_devlist_reply(r: &mut impl Read) -> Result<Vec<ExportedDevice>> {
    let (code, status) = read_header(r)?;
    expect_op(code, OP_REP_DEVLIST)?;
    if status != Status::Ok {
        return Err(Error::Refused(status));
    }
    let mut n = [0u8; 4];
    r.read_exact(&mut n)?;
    let n = u32::from_be_bytes(n);
    let mut devices = Vec::new();
    for _ in 0..n {
        let device = UsbDevice::read_from(r)?;
        let mut interfaces = Vec::with_capacity(device.num_interfaces.into());
        for _ in 0..device.num_interfaces {
            let mut b = [0u8; INTERFACE_LEN];
            r.read_exact(&mut b)?;
            interfaces.push(UsbInterface {
                class: b[0],
                subclass: b[1],
                protocol: b[2],
            });
        }
        devices.push(ExportedDevice { device, interfaces });
    }
    Ok(devices)
}

/// What a farplug server knows about its exports beyond the USB/IP device list.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerInfo {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub devices: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Manufacturer and product as the device reports them.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub in_use: bool,
    /// Address of the client using it, if the server saw the import.
    #[serde(default)]
    pub used_by: Option<String>,
}

pub fn write_info_reply(w: &mut impl Write, info: &ServerInfo) -> Result<()> {
    let json = serde_json::to_vec(info).map_err(|e| Error::BadInfo(e.to_string()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + 4 + json.len());
    put_header(&mut out, OP_REP_INFO, Status::Ok);
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    w.write_all(&out)?;
    Ok(())
}

pub fn read_info_reply(r: &mut impl Read) -> Result<ServerInfo> {
    let (code, status) = read_header(r)?;
    expect_op(code, OP_REP_INFO)?;
    if status != Status::Ok {
        return Err(Error::Refused(status));
    }
    let mut n = [0u8; 4];
    r.read_exact(&mut n)?;
    let n = u32::from_be_bytes(n);
    if n > MAX_INFO_LEN {
        return Err(Error::BadInfo(format!("{n} bytes")));
    }
    let mut json = vec![0u8; n as usize];
    r.read_exact(&mut json)?;
    serde_json::from_slice(&json).map_err(|e| Error::BadInfo(e.to_string()))
}

/// `Ok` carries the device being handed over; `Err` the refusal status.
pub fn write_import_reply(
    w: &mut impl Write,
    reply: std::result::Result<&UsbDevice, Status>,
) -> Result<()> {
    let mut out = Vec::with_capacity(HEADER_LEN + DEVICE_LEN);
    match reply {
        Ok(dev) => {
            put_header(&mut out, OP_REP_IMPORT, Status::Ok);
            dev.encode(&mut out)?;
        }
        Err(status) => put_header(&mut out, OP_REP_IMPORT, status),
    }
    w.write_all(&out)?;
    Ok(())
}

pub fn read_import_reply(r: &mut impl Read) -> Result<UsbDevice> {
    let (code, status) = read_header(r)?;
    expect_op(code, OP_REP_IMPORT)?;
    if status != Status::Ok {
        return Err(Error::Refused(status));
    }
    UsbDevice::read_from(r)
}

/// Ready a connected socket for being handed to `usbip-host` or `vhci_hcd`.
#[cfg(unix)]
pub fn prepare_for_kernel(stream: &TcpStream) -> io::Result<()> {
    // The kernel's receive loop honours SO_RCVTIMEO and would drop an idle
    // device the first time no URB arrives within the timeout.
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    stream.set_nodelay(true)?;
    // Neither kernel side has an application-level heartbeat, so without
    // keepalive a peer that reboots or drops off the network is never noticed.
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(10))
        .with_interval(Duration::from_secs(5))
        .with_retries(3);
    let sock = socket2::SockRef::from(stream);
    sock.set_tcp_keepalive(&ka)?;
    // Keepalive only probes idle connections. A device with URBs in flight
    // (storage polls constantly) would otherwise wait out the retransmission
    // limit, about 15 minutes, before a dead peer is noticed.
    #[cfg(target_os = "linux")]
    sock.set_tcp_user_timeout(Some(Duration::from_secs(30)))?;
    Ok(())
}

fn put_header(out: &mut Vec<u8>, code: u16, status: Status) {
    out.extend_from_slice(&VERSION.to_be_bytes());
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&status.as_u32().to_be_bytes());
}

fn read_header(r: &mut impl Read) -> Result<(u16, Status)> {
    let mut b = [0u8; HEADER_LEN];
    r.read_exact(&mut b)?;
    let version = u16::from_be_bytes([b[0], b[1]]);
    if version != VERSION {
        return Err(Error::Version(version));
    }
    let code = u16::from_be_bytes([b[2], b[3]]);
    let status = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    Ok((code, Status::from_u32(status)))
}

fn expect_op(got: u16, expected: u16) -> Result<()> {
    if got == expected {
        Ok(())
    } else {
        Err(Error::UnexpectedOp { got, expected })
    }
}

fn put_str(out: &mut Vec<u8>, s: &str, size: usize, field: &'static str) -> Result<()> {
    let b = s.as_bytes();
    if b.len() >= size {
        return Err(Error::TooLong {
            field,
            len: b.len(),
            max: size - 1,
        });
    }
    out.extend_from_slice(b);
    out.resize(out.len() + size - b.len(), 0);
    Ok(())
}

fn get_str(b: &[u8], field: &'static str) -> Result<String> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8(b[..end].to_vec()).map_err(|_| Error::NotUtf8(field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample() -> UsbDevice {
        UsbDevice {
            path: "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-2".into(),
            busid: "zigbee".into(),
            busnum: 1,
            devnum: 7,
            speed: Speed::Full,
            vendor_id: 0x10c4,
            product_id: 0xea60,
            bcd_device: 0x0100,
            class: 0,
            subclass: 0,
            protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 1,
        }
    }

    #[test]
    fn header_and_import_request_layout() {
        let mut out = Vec::new();
        Request::Import {
            busid: "1-1.2".into(),
        }
        .write_to(&mut out)
        .unwrap();
        assert_eq!(out.len(), 40);
        assert_eq!(&out[..8], &[0x01, 0x11, 0x80, 0x03, 0, 0, 0, 0]);
        assert_eq!(&out[8..13], b"1-1.2");
        assert!(out[13..].iter().all(|&b| b == 0));
    }

    #[test]
    fn device_field_offsets() {
        let mut out = Vec::new();
        sample().encode(&mut out).unwrap();
        assert_eq!(out.len(), DEVICE_LEN);
        assert_eq!(&out[256..262], b"zigbee");
        assert_eq!(&out[288..292], &1u32.to_be_bytes());
        assert_eq!(&out[292..296], &7u32.to_be_bytes());
        assert_eq!(&out[296..300], &2u32.to_be_bytes());
        assert_eq!(&out[300..304], &[0x10, 0xc4, 0xea, 0x60]);
        assert_eq!(out[311], 1);
    }

    #[test]
    fn devlist_roundtrip() {
        let devs = vec![ExportedDevice {
            device: sample(),
            interfaces: vec![UsbInterface {
                class: 0xff,
                subclass: 0,
                protocol: 0,
            }],
        }];
        let mut out = Vec::new();
        write_devlist_reply(&mut out, &devs).unwrap();
        assert_eq!(out.len(), 8 + 4 + DEVICE_LEN + 4);
        assert_eq!(read_devlist_reply(&mut Cursor::new(out)).unwrap(), devs);
    }

    #[test]
    fn import_reply_roundtrip_and_refusal() {
        let mut out = Vec::new();
        write_import_reply(&mut out, Ok(&sample())).unwrap();
        assert_eq!(read_import_reply(&mut Cursor::new(out)).unwrap(), sample());

        let mut out = Vec::new();
        write_import_reply(&mut out, Err(Status::DeviceBusy)).unwrap();
        assert_eq!(out.len(), 8);
        assert!(matches!(
            read_import_reply(&mut Cursor::new(out)),
            Err(Error::Refused(Status::DeviceBusy))
        ));
    }

    #[test]
    fn request_roundtrip() {
        for req in [
            Request::DevList,
            Request::Import {
                busid: "zwave".into(),
            },
        ] {
            let mut out = Vec::new();
            req.write_to(&mut out).unwrap();
            assert_eq!(Request::read_from(&mut Cursor::new(out)).unwrap(), req);
        }
    }

    #[test]
    fn rejects_wrong_version_and_long_busid() {
        let bad = [0x01, 0x06, 0x80, 0x05, 0, 0, 0, 0];
        assert!(matches!(
            Request::read_from(&mut Cursor::new(bad)),
            Err(Error::Version(0x0106))
        ));
        let long = "x".repeat(BUSID_LEN);
        assert!(Request::Import { busid: long }
            .write_to(&mut Vec::new())
            .is_err());
    }

    #[test]
    fn busid_without_terminator() {
        let mut out = Vec::new();
        put_header(&mut out, OP_REQ_IMPORT, Status::Ok);
        out.extend_from_slice(&[b'a'; BUSID_LEN]);
        let Request::Import { busid } = Request::read_from(&mut Cursor::new(out)).unwrap() else {
            panic!()
        };
        assert_eq!(busid.len(), BUSID_LEN);
    }

    #[test]
    fn devid_packs_bus_and_dev() {
        assert_eq!(sample().devid(), 0x0001_0007);
    }

    #[test]
    fn info_roundtrip() {
        let info = ServerInfo {
            version: "0.3.2".into(),
            devices: vec![DeviceInfo {
                id: "zigbee".into(),
                label: Some("SLZB-07".into()),
                description: "Silicon Labs CP210x".into(),
                in_use: true,
                used_by: Some("192.168.2.10".into()),
            }],
        };
        let mut buf = Vec::new();
        Request::Info.write_to(&mut buf).unwrap();
        assert_eq!(Request::read_from(&mut &buf[..]).unwrap(), Request::Info);
        buf.clear();
        write_info_reply(&mut buf, &info).unwrap();
        assert_eq!(read_info_reply(&mut &buf[..]).unwrap(), info);
    }
}
