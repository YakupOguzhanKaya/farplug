//! Parsing the standard USB descriptors a class driver needs to pick its
//! interface and endpoints.

use anyhow::{bail, Result};

pub const DEVICE: u8 = 1;
pub const CONFIGURATION: u8 = 2;
const INTERFACE: u8 = 4;
const ENDPOINT: u8 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub class: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub bcd_device: u16,
    pub product_index: u8,
}

impl DeviceDescriptor {
    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < 18 || b[1] != DEVICE {
            bail!("bad device descriptor ({} bytes)", b.len());
        }
        Ok(Self {
            class: b[4],
            vendor_id: u16::from_le_bytes([b[8], b[9]]),
            product_id: u16::from_le_bytes([b[10], b[11]]),
            bcd_device: u16::from_le_bytes([b[12], b[13]]),
            product_index: b[15],
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Configuration {
    pub value: u8,
    pub interfaces: Vec<Interface>,
}

/// Alternate setting 0 of an interface, which is what a fresh configuration
/// selects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub address: u8,
    pub kind: EndpointKind,
    pub max_packet: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
}

impl Interface {
    pub fn endpoint(&self, kind: EndpointKind, input: bool) -> Option<Endpoint> {
        self.endpoints
            .iter()
            .copied()
            .find(|e| e.kind == kind && e.is_in() == input)
    }
}

/// wTotalLength from the first 9 bytes of a configuration descriptor.
pub fn total_length(head: &[u8]) -> Result<u16> {
    if head.len() < 4 || head[1] != CONFIGURATION {
        bail!("bad configuration descriptor");
    }
    Ok(u16::from_le_bytes([head[2], head[3]]))
}

impl Configuration {
    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < 9 || b[1] != CONFIGURATION {
            bail!("bad configuration descriptor");
        }
        let mut cfg = Self {
            value: b[5],
            interfaces: Vec::new(),
        };
        let mut current_alt = 0;
        let mut i = usize::from(b[0]);
        while i + 2 <= b.len() {
            let len = usize::from(b[i]);
            if len < 2 || i + len > b.len() {
                break;
            }
            let d = &b[i..i + len];
            match d[1] {
                INTERFACE if len >= 9 => {
                    current_alt = d[3];
                    if current_alt == 0 {
                        cfg.interfaces.push(Interface {
                            number: d[2],
                            class: d[5],
                            subclass: d[6],
                            protocol: d[7],
                            endpoints: Vec::new(),
                        });
                    }
                }
                ENDPOINT if len >= 7 && current_alt == 0 => {
                    if let Some(iface) = cfg.interfaces.last_mut() {
                        iface.endpoints.push(Endpoint {
                            address: d[2],
                            kind: match d[3] & 0x03 {
                                0 => EndpointKind::Control,
                                1 => EndpointKind::Isochronous,
                                2 => EndpointKind::Bulk,
                                _ => EndpointKind::Interrupt,
                            },
                            max_packet: u16::from_le_bytes([d[4], d[5]]) & 0x07ff,
                        });
                    }
                }
                _ => {}
            }
            i += len;
        }
        Ok(cfg)
    }
}

pub fn string(b: &[u8]) -> Result<String> {
    if b.len() < 2 || b[1] != 3 {
        bail!("bad string descriptor");
    }
    let units: Vec<u16> = b[2..usize::from(b[0]).min(b.len())]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_cdc_acm_configuration() {
        // Config, IAD, comm interface (+ functional descriptors, interrupt
        // IN), data interface (bulk OUT, bulk IN).
        let cfg: Vec<u8> = [
            &[9u8, 2, 67, 0, 2, 1, 0, 0x80, 50][..],
            &[9, 4, 0, 0, 1, 2, 2, 1, 0],
            &[5, 0x24, 0, 0x10, 1],
            &[7, 5, 0x83, 3, 8, 0, 16],
            &[9, 4, 1, 0, 2, 0x0a, 0, 0, 0],
            &[7, 5, 0x02, 2, 64, 0, 0],
            &[7, 5, 0x81, 2, 64, 0, 0],
            &[9, 4, 1, 1, 0, 0x0a, 0, 0, 0],
        ]
        .concat();
        let c = Configuration::parse(&cfg).unwrap();
        assert_eq!(c.value, 1);
        assert_eq!(c.interfaces.len(), 2);
        let data = &c.interfaces[1];
        assert_eq!((data.number, data.class), (1, 0x0a));
        assert_eq!(
            data.endpoint(EndpointKind::Bulk, true).unwrap().address,
            0x81
        );
        assert_eq!(
            data.endpoint(EndpointKind::Bulk, false).unwrap().address,
            0x02
        );
        assert_eq!(
            c.interfaces[0]
                .endpoint(EndpointKind::Interrupt, true)
                .unwrap()
                .max_packet,
            8
        );
        assert_eq!(string(&[8, 3, b'a', 0, b'b', 0, b'c', 0]).unwrap(), "abc");
    }
}
