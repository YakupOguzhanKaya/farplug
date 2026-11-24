//! A USB host in userspace: drives an imported device over the USB/IP
//! connection itself, for systems without a virtual host controller in the
//! kernel (macOS). Class drivers ([`crate::serial`]) build on [`Device`].

pub mod descriptors;

use std::collections::HashMap;
use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use farplug_usbip::urb::{self, Reply, Submit, DIR_IN, DIR_OUT};
use farplug_usbip::{self as usbip, Request};

use crate::server::Server;
use descriptors::{Configuration, DeviceDescriptor, Endpoint};

pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const ECONNRESET: i32 = 104;
const EPIPE: i32 = 32;

/// An imported device whose transfers this process makes.
pub struct Device {
    stream: TcpStream,
    writer: Mutex<TcpStream>,
    shared: Arc<Shared>,
    next_seq: AtomicU32,
    pub devid: u32,
    /// High speed and faster count interrupt intervals in microframes.
    high_speed: bool,
    pub descriptor: DeviceDescriptor,
    pub config: Configuration,
    pub product: Option<String>,
}

/// Whether the transfer is IN, and where its reply goes (None once given up).
type Pending = (bool, Option<Sender<Reply>>);

#[derive(Default)]
struct Shared {
    /// seqnum -> (is IN, where its reply goes). Kept until the reply or a
    /// successful unlink, because only IN replies carry data and the reader
    /// must know how much to read.
    pending: Mutex<HashMap<u32, Pending>>,
    /// Unlink seqnum -> the transfer it cancels.
    unlinks: Mutex<HashMap<u32, u32>>,
    closed: AtomicBool,
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("the device stalled the request")]
    Stall,
    #[error("timed out")]
    Timeout,
    #[error("the connection to the server closed")]
    Closed,
    #[error("transfer failed with error {0}")]
    Status(i32),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl Device {
    /// Imports `busid` from `server` and configures it.
    pub fn open(server: &Server, busid: &str) -> Result<Self> {
        let mut sock = server.connect()?;
        Request::Import {
            busid: busid.to_owned(),
        }
        .write_to(&mut sock)?;
        let udev = usbip::read_import_reply(&mut sock)
            .with_context(|| format!("importing {busid} from {server}"))?;
        sock.set_read_timeout(None)?;
        sock.set_write_timeout(None)?;
        #[cfg(unix)]
        usbip::prepare_for_kernel(&sock)?;

        let shared = Arc::new(Shared::default());
        let reader = sock.try_clone()?;
        let s = shared.clone();
        thread::spawn(move || read_replies(reader, &s));

        let mut dev = Self {
            writer: Mutex::new(sock.try_clone()?),
            stream: sock,
            shared,
            next_seq: AtomicU32::new(1),
            devid: udev.devid(),
            high_speed: matches!(
                udev.speed,
                usbip::Speed::High | usbip::Speed::Super | usbip::Speed::SuperPlus
            ),
            descriptor: DeviceDescriptor::default(),
            config: Configuration::default(),
            product: None,
        };
        dev.enumerate()
            .with_context(|| format!("setting up {busid}"))?;
        Ok(dev)
    }

    fn enumerate(&mut self) -> Result<()> {
        let raw = self.get_descriptor(descriptors::DEVICE, 0, 18)?;
        self.descriptor = DeviceDescriptor::parse(&raw)?;
        let head = self.get_descriptor(descriptors::CONFIGURATION, 0, 9)?;
        let total = descriptors::total_length(&head)?;
        let raw = self.get_descriptor(descriptors::CONFIGURATION, 0, total)?;
        self.config = Configuration::parse(&raw)?;
        // The server's usbip-host carries this out on the real device.
        self.control_out(0x00, 9, self.config.value.into(), 0, &[])
            .context("selecting the configuration")?;
        if self.descriptor.product_index != 0 {
            self.product = self.string(self.descriptor.product_index).ok();
        }
        Ok(())
    }

    fn get_descriptor(&self, kind: u8, index: u8, len: u16) -> Result<Vec<u8>> {
        Ok(self.control_in(0x80, 6, u16::from(kind) << 8 | u16::from(index), 0, len)?)
    }

    fn string(&self, index: u8) -> Result<String> {
        let raw = self.control_in(0x80, 6, 0x0300 | u16::from(index), 0x0409, 255)?;
        descriptors::string(&raw)
    }

    pub fn control_in(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
    ) -> Result<Vec<u8>, TransferError> {
        let setup = urb::setup(request_type | 0x80, request, value, index, len);
        self.submit(
            Submit {
                length: len.into(),
                setup,
                ..transfer(DIR_IN, 0)
            },
            Some(CONTROL_TIMEOUT),
        )
    }

    pub fn control_out(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), TransferError> {
        let setup = urb::setup(
            request_type & 0x7f,
            request,
            value,
            index,
            data.len() as u16,
        );
        self.submit(
            Submit {
                data: data.to_vec(),
                setup,
                ..transfer(DIR_OUT, 0)
            },
            Some(CONTROL_TIMEOUT),
        )
        .map(drop)
    }

    /// A bulk IN transfer of up to `len` bytes. Without a timeout it waits
    /// until data arrives or the device is closed.
    pub fn read(
        &self,
        ep: u8,
        len: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, TransferError> {
        self.submit(
            Submit {
                length: len,
                ..transfer(DIR_IN, ep)
            },
            timeout,
        )
    }

    /// One report from an interrupt IN endpoint, waiting until there is one.
    pub fn read_interrupt(&self, ep: &Endpoint) -> Result<Vec<u8>, TransferError> {
        // Linux refuses interrupt transfers without a polling interval; send
        // it as Linux's own USB/IP client does: frames, or microframes from
        // high speed up.
        let interval = if self.high_speed {
            1u32 << (ep.interval.clamp(1, 16) - 1)
        } else {
            u32::from(ep.interval.max(1))
        };
        self.submit(
            Submit {
                length: usize::from(ep.max_packet),
                interval,
                ..transfer(DIR_IN, ep.address)
            },
            None,
        )
    }

    pub fn write(
        &self,
        ep: u8,
        data: &[u8],
        timeout: Option<Duration>,
    ) -> Result<(), TransferError> {
        self.submit(
            Submit {
                data: data.to_vec(),
                ..transfer(DIR_OUT, ep)
            },
            timeout,
        )
        .map(drop)
    }

    /// False once the server or network went away.
    pub fn is_alive(&self) -> bool {
        !self.shared.closed.load(Ordering::Relaxed)
    }

    /// Ends the connection; pending and later transfers fail with
    /// [`TransferError::Closed`].
    pub fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Sends `submit` (its seqnum and devid filled in here) and waits for
    /// the reply.
    fn submit(
        &self,
        mut submit: Submit,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, TransferError> {
        if !self.is_alive() {
            return Err(TransferError::Closed);
        }
        let seqnum = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.shared
            .pending
            .lock()
            .unwrap()
            .insert(seqnum, (submit.direction == DIR_IN, Some(tx)));
        submit.seqnum = seqnum;
        submit.devid = self.devid;
        if let Err(e) = submit.write_to(&mut *self.writer.lock().unwrap()) {
            self.shared.pending.lock().unwrap().remove(&seqnum);
            return Err(e.into());
        }
        match wait(&rx, timeout) {
            Ok(Reply::Submitted {
                status: 0, data, ..
            }) => Ok(data),
            Ok(Reply::Submitted { status, .. }) if -status == EPIPE => Err(TransferError::Stall),
            Ok(Reply::Submitted { status, .. }) => Err(TransferError::Status(status)),
            Ok(Reply::Unlinked { .. }) | Err(RecvTimeoutError::Disconnected) => {
                Err(TransferError::Closed)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.cancel(seqnum);
                Err(TransferError::Timeout)
            }
        }
    }

    fn cancel(&self, victim: u32) {
        if let Some(entry) = self.shared.pending.lock().unwrap().get_mut(&victim) {
            entry.1 = None;
        }
        let seqnum = self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.shared.unlinks.lock().unwrap().insert(seqnum, victim);
        let _ = urb::write_unlink(
            &mut *self.writer.lock().unwrap(),
            seqnum,
            self.devid,
            victim,
        );
    }
}

/// A transfer on endpoint `ep` (its number; the direction bit is dropped)
/// with nothing to send, nothing to receive, and no setup packet yet.
fn transfer(direction: u32, ep: u8) -> Submit {
    Submit {
        seqnum: 0,
        devid: 0,
        direction,
        ep: (ep & 0x0f).into(),
        data: Vec::new(),
        length: 0,
        interval: 0,
        setup: [0; 8],
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.close();
    }
}

fn wait(rx: &Receiver<Reply>, timeout: Option<Duration>) -> Result<Reply, RecvTimeoutError> {
    match timeout {
        Some(t) => rx.recv_timeout(t),
        None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
    }
}

fn read_replies(mut stream: TcpStream, shared: &Shared) {
    loop {
        let is_in = |seq| {
            shared
                .pending
                .lock()
                .unwrap()
                .get(&seq)
                .is_some_and(|p| p.0)
        };
        match urb::read_reply(&mut stream, is_in) {
            Ok(reply @ Reply::Submitted { seqnum, .. }) => {
                let entry = shared.pending.lock().unwrap().remove(&seqnum);
                if let Some((_, Some(tx))) = entry {
                    let _ = tx.send(reply);
                }
            }
            Ok(Reply::Unlinked { seqnum, status }) => {
                let victim = shared.unlinks.lock().unwrap().remove(&seqnum);
                // ECONNRESET: cancelled before it finished, so no reply for it
                // will follow. Otherwise its reply was already handled.
                if let (Some(v), true) = (victim, -status == ECONNRESET) {
                    shared.pending.lock().unwrap().remove(&v);
                }
            }
            Err(_) => break,
        }
    }
    shared.closed.store(true, Ordering::Relaxed);
    // Dropping the senders wakes every waiting transfer.
    shared.pending.lock().unwrap().clear();
}
