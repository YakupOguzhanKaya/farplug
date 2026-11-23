//! USB/IP data phase: the transfer messages that follow a successful import.
//! On Linux the kernel speaks these; the userspace client (macOS) speaks them
//! itself. Every header is 48 bytes, big-endian.

use std::io::{self, Read, Write};

pub const CMD_SUBMIT: u32 = 1;
pub const CMD_UNLINK: u32 = 2;
pub const RET_SUBMIT: u32 = 3;
pub const RET_UNLINK: u32 = 4;

pub const DIR_OUT: u32 = 0;
pub const DIR_IN: u32 = 1;

const HEADER_LEN: usize = 48;
/// URB_DIR_IN, which the Linux client sets on IN transfers.
const URB_DIR_IN: u32 = 0x0200;
/// More than any transfer a serial or HID driver asks for; guards against a
/// corrupt length.
const MAX_TRANSFER: usize = 1 << 20;

/// One transfer request. `setup` is only meaningful on endpoint 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
    /// Bytes to send (OUT); empty for IN.
    pub data: Vec<u8>,
    /// Bytes to receive (IN).
    pub length: usize,
    pub interval: u32,
    pub setup: [u8; 8],
}

impl Submit {
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        let length = if self.direction == DIR_IN {
            self.length
        } else {
            self.data.len()
        };
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        for v in [CMD_SUBMIT, self.seqnum, self.devid, self.direction, self.ep] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        let flags = if self.direction == DIR_IN {
            URB_DIR_IN
        } else {
            0
        };
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&(length as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // start_frame
        out.extend_from_slice(&0u32.to_be_bytes()); // number_of_packets: not ISO
        out.extend_from_slice(&self.interval.to_be_bytes());
        out.extend_from_slice(&self.setup);
        if self.direction == DIR_OUT {
            out.extend_from_slice(&self.data);
        }
        w.write_all(&out)
    }
}

/// Ask the server to cancel the transfer `victim`; it answers with a
/// [`Reply::Unlinked`] and, if the transfer was still pending, no
/// [`Reply::Submitted`] for it.
pub fn write_unlink(w: &mut impl Write, seqnum: u32, devid: u32, victim: u32) -> io::Result<()> {
    let mut out = Vec::with_capacity(HEADER_LEN);
    for v in [CMD_UNLINK, seqnum, devid, DIR_OUT, 0, victim] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.resize(HEADER_LEN, 0);
    w.write_all(&out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// A finished transfer: status 0 on success, a negative errno otherwise
    /// (-32, EPIPE, is a stall). `data` holds what an IN transfer received.
    Submitted {
        seqnum: u32,
        status: i32,
        data: Vec<u8>,
    },
    Unlinked {
        seqnum: u32,
        status: i32,
    },
}

/// Reads one reply. `is_in` tells whether the transfer with a given seqnum
/// was an IN transfer: the reply header does not say, and only IN replies
/// carry data.
pub fn read_reply(r: &mut impl Read, is_in: impl Fn(u32) -> bool) -> io::Result<Reply> {
    let mut h = [0u8; HEADER_LEN];
    r.read_exact(&mut h)?;
    let word = |i: usize| u32::from_be_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    let (command, seqnum, status) = (word(0), word(4), word(20) as i32);
    match command {
        RET_SUBMIT => {
            let actual = word(24) as usize;
            let number_of_packets = word(32);
            if actual > MAX_TRANSFER {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("transfer of {actual} bytes"),
                ));
            }
            let mut data = Vec::new();
            if is_in(seqnum) && actual > 0 {
                data = vec![0; actual];
                r.read_exact(&mut data)?;
            }
            // Isochronous descriptors would follow; this client never asks
            // for isochronous transfers, so their presence means a bad peer.
            if number_of_packets != 0 && number_of_packets != u32::MAX {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected isochronous reply",
                ));
            }
            Ok(Reply::Submitted {
                seqnum,
                status,
                data,
            })
        }
        RET_UNLINK => Ok(Reply::Unlinked { seqnum, status }),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected USB/IP command {other:#x}"),
        )),
    }
}

/// A control transfer's 8-byte setup packet.
pub fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> [u8; 8] {
    let mut s = [0u8; 8];
    s[0] = request_type;
    s[1] = request;
    s[2..4].copy_from_slice(&value.to_le_bytes());
    s[4..6].copy_from_slice(&index.to_le_bytes());
    s[6..8].copy_from_slice(&length.to_le_bytes());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_layout_matches_the_spec() {
        let s = Submit {
            seqnum: 7,
            devid: 0x0003_0009,
            direction: DIR_OUT,
            ep: 2,
            data: vec![0xaa, 0xbb],
            length: 0,
            interval: 0,
            setup: [0; 8],
        };
        let mut buf = Vec::new();
        s.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), 48 + 2);
        assert_eq!(&buf[0..4], &1u32.to_be_bytes());
        assert_eq!(&buf[4..8], &7u32.to_be_bytes());
        assert_eq!(&buf[8..12], &0x0003_0009u32.to_be_bytes());
        assert_eq!(&buf[16..20], &2u32.to_be_bytes());
        assert_eq!(&buf[24..28], &2u32.to_be_bytes()); // transfer_buffer_length
        assert_eq!(&buf[48..], &[0xaa, 0xbb]);
        assert_eq!(setup(0x80, 6, 0x0100, 0, 18), [0x80, 6, 0, 1, 0, 0, 18, 0]);
    }

    #[test]
    fn reads_replies_with_and_without_data() {
        let mut wire = Vec::new();
        for v in [RET_SUBMIT, 5, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0] {
            wire.extend_from_slice(&v.to_be_bytes());
        }
        wire.extend_from_slice(b"abc");
        for v in [RET_SUBMIT, 6, 0, 0, 0, (-32i32) as u32, 0, 0, 0, 0, 0, 0] {
            wire.extend_from_slice(&v.to_be_bytes());
        }
        let mut r = &wire[..];
        assert_eq!(
            read_reply(&mut r, |s| s == 5).unwrap(),
            Reply::Submitted {
                seqnum: 5,
                status: 0,
                data: b"abc".to_vec()
            }
        );
        assert_eq!(
            read_reply(&mut r, |s| s == 5).unwrap(),
            Reply::Submitted {
                seqnum: 6,
                status: -32,
                data: vec![]
            }
        );
    }
}
