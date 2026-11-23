//! The USB-serial chips farplug can drive itself: how each sets its line
//! settings and modem lines, and which endpoints carry the data.

use anyhow::{bail, Context, Result};

use crate::userhost::descriptors::{EndpointKind, Interface};
use crate::userhost::Device;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    None,
    Odd,
    Even,
    Mark,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    /// 1 or 2.
    pub stop_bits: u8,
}

impl Default for Line {
    fn default() -> Self {
        Self {
            baud: 9600,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    CdcAcm,
    Cp210x,
    Ch34x,
    Ftdi,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::CdcAcm => "CDC-ACM",
            Self::Cp210x => "CP210x",
            Self::Ch34x => "CH34x",
            Self::Ftdi => "FTDI",
        }
    }
}

/// A recognized serial chip, with the interface and endpoints it uses.
#[derive(Debug, Clone)]
pub struct Chip {
    pub kind: Kind,
    /// Where class or vendor requests go.
    control_iface: u8,
    pub bulk_in: u8,
    pub bulk_out: u8,
    pub in_packet: usize,
    /// CH34x chip version, which changes how line settings are written.
    ch34x_version: u8,
    /// FTDI request index: 0 on single-port chips, the port (1-based) on
    /// multi-port ones.
    ftdi_index: u16,
    ftdi_high_speed: bool,
}

const FTDI_VID: u16 = 0x0403;
const CP210X_VID: u16 = 0x10c4;
const WCH_VID: u16 = 0x1a86;

impl Chip {
    /// None if the device is not a serial adapter farplug knows.
    pub fn detect(dev: &Device) -> Option<Self> {
        let ifaces = &dev.config.interfaces;
        let bulk = |i: &Interface| {
            let (r, w) = (
                i.endpoint(EndpointKind::Bulk, true)?,
                i.endpoint(EndpointKind::Bulk, false)?,
            );
            Some((r.address, w.address, usize::from(r.max_packet)))
        };
        let chip = |kind, control_iface, (bulk_in, bulk_out, in_packet)| Self {
            kind,
            control_iface,
            bulk_in,
            bulk_out,
            in_packet,
            ch34x_version: 0,
            ftdi_index: 0,
            ftdi_high_speed: false,
        };
        // CDC-ACM: a communications interface (2/2) and a data interface (10)
        // with the bulk pipes. Class compliant, so check it first.
        if let (Some(comm), Some(data)) = (
            ifaces.iter().find(|i| i.class == 2 && i.subclass == 2),
            ifaces.iter().find(|i| i.class == 0x0a),
        ) {
            return Some(chip(Kind::CdcAcm, comm.number, bulk(data)?));
        }
        let d = &dev.descriptor;
        let vendor = ifaces
            .iter()
            .find(|i| i.class == 0xff && bulk(i).is_some())?;
        let kind = match d.vendor_id {
            CP210X_VID => Kind::Cp210x,
            WCH_VID => Kind::Ch34x,
            FTDI_VID => Kind::Ftdi,
            _ => return None,
        };
        let mut c = chip(kind, vendor.number, bulk(vendor)?);
        if kind == Kind::Ftdi {
            // FT2232/FT4232: index each port; FT2232H, FT4232H and FT232H
            // run their baud generator from a faster clock.
            let multi_port = ifaces.len() > 1;
            c.ftdi_index = if multi_port {
                u16::from(vendor.number) + 1
            } else {
                0
            };
            c.ftdi_high_speed = matches!(dev.descriptor.bcd_device, 0x0700 | 0x0800 | 0x0900);
        }
        Some(c)
    }

    pub fn init(&mut self, dev: &Device, line: &Line) -> Result<()> {
        match self.kind {
            Kind::CdcAcm => {}
            Kind::Cp210x => {
                // IFC_ENABLE
                dev.control_out(0x41, 0x00, 1, self.control_iface.into(), &[])?;
            }
            Kind::Ch34x => {
                let v = dev.control_in(0xc0, 0x5f, 0, 0, 2)?;
                self.ch34x_version = v.first().copied().unwrap_or(0);
                dev.control_out(0x40, 0xa1, 0, 0, &[])?; // SERIAL_INIT
            }
            Kind::Ftdi => {
                dev.control_out(0x40, 0x00, 0, self.ftdi_index, &[])?; // RESET
                dev.control_out(0x40, 0x02, 0, self.ftdi_index, &[])?; // no flow control
            }
        }
        self.set_line(dev, line)
            .with_context(|| format!("setting up the {} chip", self.kind.name()))?;
        // DTR and RTS on: many devices only talk once they see them, and a pty
        // cannot pass an app's own changes to them along.
        self.set_modem(dev, true, true)
    }

    pub fn set_line(&self, dev: &Device, l: &Line) -> Result<()> {
        if l.baud == 0 {
            bail!("baud rate 0");
        }
        let iface = u16::from(self.control_iface);
        match self.kind {
            Kind::CdcAcm => {
                let mut coding = l.baud.to_le_bytes().to_vec();
                coding.push(if l.stop_bits == 2 { 2 } else { 0 });
                coding.push(parity_code(l.parity));
                coding.push(l.data_bits);
                dev.control_out(0x21, 0x20, 0, iface, &coding)?; // SET_LINE_CODING
            }
            Kind::Cp210x => {
                dev.control_out(0x41, 0x1e, 0, iface, &l.baud.to_le_bytes())?; // SET_BAUDRATE
                let stop = if l.stop_bits == 2 { 2 } else { 0 };
                let ctl =
                    stop | u16::from(parity_code(l.parity)) << 4 | u16::from(l.data_bits) << 8;
                dev.control_out(0x41, 0x03, ctl, iface, &[])?; // SET_LINE_CTL
            }
            Kind::Ch34x => {
                let mut divisor = ch34x_divisor(l.baud)?;
                if self.ch34x_version > 0x27 {
                    divisor |= 1 << 7;
                }
                dev.control_out(0x40, 0x9a, 0x1312, divisor, &[])?; // prescaler, divisor
                if self.ch34x_version >= 0x30 {
                    dev.control_out(0x40, 0x9a, 0x2518, ch34x_lcr(l).into(), &[])?;
                }
            }
            Kind::Ftdi => {
                let d = if self.ftdi_high_speed && l.baud >= 1200 {
                    ftdi_divisor_high_speed(l.baud)
                } else {
                    ftdi_divisor(l.baud)
                };
                let index = if self.ftdi_index == 0 {
                    (d >> 16) as u16
                } else {
                    ((d >> 16) as u16) << 8 | self.ftdi_index
                };
                dev.control_out(0x40, 0x03, d as u16, index, &[])?; // SET_BAUD_RATE
                let stop: u16 = if l.stop_bits == 2 { 2 } else { 0 };
                let data =
                    u16::from(l.data_bits) | u16::from(parity_code(l.parity)) << 8 | stop << 11;
                dev.control_out(0x40, 0x04, data, self.ftdi_index, &[])?; // SET_DATA
            }
        }
        Ok(())
    }

    pub fn set_modem(&self, dev: &Device, dtr: bool, rts: bool) -> Result<()> {
        let (dtr, rts) = (u16::from(dtr), u16::from(rts));
        match self.kind {
            Kind::CdcAcm => {
                // SET_CONTROL_LINE_STATE
                dev.control_out(0x21, 0x22, dtr | rts << 1, self.control_iface.into(), &[])?
            }
            Kind::Cp210x => {
                // SET_MHS, with the mask bits saying both lines are set.
                dev.control_out(
                    0x41,
                    0x07,
                    dtr | rts << 1 | 0x0300,
                    self.control_iface.into(),
                    &[],
                )?
            }
            Kind::Ch34x => dev.control_out(0x40, 0xa4, !(dtr << 5 | rts << 6), 0, &[])?,
            Kind::Ftdi => {
                dev.control_out(0x40, 0x01, 0x0300 | dtr | rts << 1, self.ftdi_index, &[])?
            }
        }
        Ok(())
    }

    /// The payload of what a bulk IN transfer returned: FTDI chips start
    /// every packet with two modem status bytes.
    pub fn payload(&self, data: Vec<u8>) -> Vec<u8> {
        if self.kind != Kind::Ftdi || self.in_packet == 0 {
            return data;
        }
        data.chunks(self.in_packet)
            .flat_map(|p| p.get(2..).unwrap_or_default().iter().copied())
            .collect()
    }
}

fn parity_code(p: Parity) -> u8 {
    match p {
        Parity::None => 0,
        Parity::Odd => 1,
        Parity::Even => 2,
        Parity::Mark => 3,
        Parity::Space => 4,
    }
}

const CH34X_CLOCK: u32 = 48_000_000;

/// Prescaler and divisor register value; see Linux's ch341.c.
fn ch34x_divisor(baud: u32) -> Result<u16> {
    let speed = baud.clamp(46, 2_000_000);
    let clk_div = |ps: u32, fact: u32| 1u32 << (12 - 3 * ps - fact);
    let min_rate = |ps: u32| CH34X_CLOCK / (clk_div(ps, 1) * 512);
    let Some(ps) = (0..=3u32).rev().find(|&ps| speed > min_rate(ps)) else {
        bail!("{baud} baud is too slow for a CH34x");
    };
    let mut fact = 1;
    let mut cd = clk_div(ps, fact);
    let mut div = CH34X_CLOCK / (cd * speed);
    if !(9..=255).contains(&div) {
        div /= 2;
        cd *= 2;
        fact = 0;
    }
    if div < 2 {
        bail!("{baud} baud is too fast for a CH34x");
    }
    // Round to the nearer of div and div + 1, scaled to keep precision.
    if 16 * CH34X_CLOCK / (cd * div) - 16 * speed
        >= 16 * speed - 16 * CH34X_CLOCK / (cd * (div + 1))
    {
        div += 1;
    }
    // Prefer the lower base clock when the divisor is even (CH341A errata).
    if fact == 1 && div.is_multiple_of(2) {
        div /= 2;
        fact = 0;
    }
    Ok(((0x100 - div) << 8 | fact << 2 | ps) as u16)
}

fn ch34x_lcr(l: &Line) -> u8 {
    let mut lcr = 0xc0 | (l.data_bits.clamp(5, 8) - 5); // RX and TX enabled
    if l.stop_bits == 2 {
        lcr |= 0x04;
    }
    lcr |= match l.parity {
        Parity::None => 0,
        Parity::Odd => 0x08,
        Parity::Even => 0x18,
        Parity::Mark => 0x28,
        Parity::Space => 0x38,
    };
    lcr
}

const FTDI_FRACTIONS: [u32; 8] = [0, 3, 2, 4, 1, 5, 6, 7];

/// FT232R, FT-X and FT232BM; see Linux's ftdi_sio.c.
fn ftdi_divisor(baud: u32) -> u32 {
    let div3 = (48_000_000 + baud) / (2 * baud);
    ftdi_special((div3 >> 3) | FTDI_FRACTIONS[(div3 & 7) as usize] << 14)
}

/// The high-speed chips' 120 MHz clock, with the bit that disables its
/// divide-by-2.5.
fn ftdi_divisor_high_speed(baud: u32) -> u32 {
    let div3 = (8 * 120_000_000 + 5 * baud) / (10 * baud);
    ftdi_special((div3 >> 3) | FTDI_FRACTIONS[(div3 & 7) as usize] << 14) | 0x0002_0000
}

fn ftdi_special(d: u32) -> u32 {
    match d {
        1 => 0,
        0x4001 => 1,
        d => d,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ch34x_divisors_match_linux() {
        // Values the Linux ch341 driver computes for these rates.
        assert_eq!(ch34x_divisor(9600).unwrap(), 0xb202);
        assert_eq!(ch34x_divisor(115_200).unwrap(), 0xcc03);
        assert_eq!(ch34x_divisor(921_600).unwrap(), 0xf307);
    }

    #[test]
    fn ftdi_divisors_match_linux() {
        assert_eq!(ftdi_divisor(9600), 0x4138);
        assert_eq!(ftdi_divisor(115_200), 0x001a);
        assert_eq!(ftdi_divisor(3_000_000), 0);
        assert_eq!(ftdi_divisor_high_speed(115_200), 0x0002_c068);
    }

    #[test]
    fn ftdi_payload_strips_status_bytes() {
        let chip = Chip {
            kind: Kind::Ftdi,
            control_iface: 0,
            bulk_in: 0x81,
            bulk_out: 0x02,
            in_packet: 4,
            ch34x_version: 0,
            ftdi_index: 0,
            ftdi_high_speed: false,
        };
        assert_eq!(
            chip.payload(vec![0x31, 0x60, b'a', b'b', 0x31, 0x60, b'c']),
            b"abc"
        );
    }
}
