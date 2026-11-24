//! A remote USB-serial adapter as a local serial port: the chip is driven
//! over USB/IP ([`chips`]) and its data copied to and from a pseudo-terminal,
//! which any program can open like a real port. A symlink with a stable name
//! points at the pty, whose own name changes every time.

pub mod chips;

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::{debug, warn};

use crate::userhost::{Device, TransferError};
use chips::{Chip, Line, Parity};

const WATCH: Duration = Duration::from_millis(200);
const READ_LEN: usize = 4096;

pub struct Port {
    dev: Arc<Device>,
    stop: Arc<AtomicBool>,
    link: PathBuf,
    pub tty: String,
    pub chip: chips::Kind,
}

impl Port {
    /// Sets up the chip and starts copying data; the port lives until
    /// [`Port::stop`] or until the device goes away.
    pub fn start(dev: Device, link: &Path) -> Result<Self> {
        let Some(mut chip) = Chip::detect(&dev) else {
            bail!(
                "{:04x}:{:04x} ({}) is not a serial adapter farplug can drive on this OS",
                dev.descriptor.vendor_id,
                dev.descriptor.product_id,
                dev.product.as_deref().unwrap_or("unknown device"),
            );
        };
        let (master, slave, tty) = open_pty()?;
        share(&tty);
        set_raw(&slave)?;
        let mut line = line_of(&slave).unwrap_or_default();
        chip.init(&dev, &line)?;

        if let Some(dir) = link.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let _ = fs::remove_file(link);
        std::os::unix::fs::symlink(&tty, link)
            .with_context(|| format!("linking {} to {tty}", link.display()))?;

        let dev = Arc::new(dev);
        let stop = Arc::new(AtomicBool::new(false));
        let master = Arc::new(master);
        let kind = chip.kind;
        let chip = Arc::new(chip);

        let (d, c, m, s) = (dev.clone(), chip.clone(), master.clone(), stop.clone());
        thread::spawn(move || {
            let mut out = File::from(m.try_clone().expect("duplicating the pty"));
            let len = READ_LEN.max(c.in_packet);
            while !s.load(Ordering::Relaxed) {
                match d.read(c.bulk_in, len, None) {
                    Ok(data) => {
                        let data = c.payload(data);
                        if !data.is_empty() && out.write_all(&data).is_err() {
                            break;
                        }
                    }
                    Err(TransferError::Closed) => break,
                    Err(e) => {
                        warn!(error = %e, "reading from the serial adapter failed");
                        break;
                    }
                }
            }
            s.store(true, Ordering::Relaxed);
        });

        let (d, c, m, s) = (dev.clone(), chip.clone(), master.clone(), stop.clone());
        thread::spawn(move || {
            let mut input = File::from(m.try_clone().expect("duplicating the pty"));
            let mut buf = [0u8; READ_LEN];
            while !s.load(Ordering::Relaxed) {
                // Wake up now and then to notice `stop`.
                if !readable(&input, WATCH) {
                    continue;
                }
                match input.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Err(e) = d.write(c.bulk_out, &buf[..n], None) {
                            debug!(error = %e, "serial write ended");
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            s.store(true, Ordering::Relaxed);
        });

        let (d, c, s) = (dev.clone(), chip.clone(), stop.clone());
        thread::spawn(move || {
            // Programs set the baud rate on their end of the pty; pass each
            // change on to the chip.
            while !s.load(Ordering::Relaxed) && d.is_alive() {
                thread::sleep(WATCH);
                if let Some(now) = line_of(&slave) {
                    if now != line {
                        match c.set_line(&d, &now) {
                            Ok(()) => debug!(?now, "line settings changed"),
                            Err(e) => {
                                warn!(error = %format!("{e:#}"), "changing line settings failed")
                            }
                        }
                        line = now;
                    }
                }
            }
            s.store(true, Ordering::Relaxed);
            d.close();
            drop(slave);
        });

        Ok(Self {
            dev,
            stop,
            link: link.to_owned(),
            tty,
            chip: kind,
        })
    }

    pub fn link(&self) -> &Path {
        &self.link
    }

    pub fn is_alive(&self) -> bool {
        !self.stop.load(Ordering::Relaxed) && self.dev.is_alive()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.dev.close();
        // Only remove the link if it still points at this port's pty.
        if fs::read_link(&self.link).is_ok_and(|t| t == Path::new(&self.tty)) {
            let _ = fs::remove_file(&self.link);
        }
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A pty opened by the root agent belongs to root; hand it to the farplug
/// group (or, without one, everyone) so its users can open the port.
fn share(tty: &str) {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let mode = match crate::auth::group_id() {
        Some(gid) => {
            let _ = std::os::unix::fs::chown(tty, Some(0), Some(gid));
            0o660
        }
        None => 0o666,
    };
    let _ = fs::set_permissions(tty, fs::Permissions::from_mode(mode));
}

fn open_pty() -> Result<(OwnedFd, OwnedFd, String)> {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty writes two descriptors into the pointers it is given;
    // the name, termios and window size arguments may be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("opening a pseudo-terminal");
    }
    // SAFETY: both descriptors are fresh and owned by nothing else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = tty_name(&slave)?;
    Ok((master, slave, tty))
}

fn tty_name(fd: &OwnedFd) -> Result<String> {
    let mut buf = [0 as libc::c_char; 128];
    // SAFETY: buf is valid for its length and ttyname_r NUL-terminates it.
    let rc = unsafe { libc::ttyname_r(fd.as_raw_fd(), buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        bail!("naming the pseudo-terminal: error {rc}");
    }
    // SAFETY: ttyname_r succeeded, so buf holds a NUL-terminated string.
    let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Ok(name.to_string_lossy().into_owned())
}

fn termios(fd: &OwnedFd) -> Option<libc::termios> {
    let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: tcgetattr fills the struct on success.
    (unsafe { libc::tcgetattr(fd.as_raw_fd(), t.as_mut_ptr()) } == 0)
        .then(|| unsafe { t.assume_init() })
}

/// No echo or line editing, so binary protocols pass through untouched.
fn set_raw(fd: &OwnedFd) -> Result<()> {
    let mut t = termios(fd).context("reading the pty's settings")?;
    // SAFETY: t is a valid termios from tcgetattr.
    unsafe {
        libc::cfmakeraw(&mut t);
        libc::cfsetspeed(&mut t, libc::B9600);
    }
    // SAFETY: as above.
    if unsafe { libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &t) } != 0 {
        return Err(std::io::Error::last_os_error()).context("setting the pty to raw mode");
    }
    Ok(())
}

fn line_of(fd: &OwnedFd) -> Option<Line> {
    let t = termios(fd)?;
    // SAFETY: t is a valid termios.
    let speed = unsafe { libc::cfgetospeed(&t) };
    let flags = t.c_cflag;
    Some(Line {
        baud: baud(speed)?,
        data_bits: match flags & libc::CSIZE {
            libc::CS5 => 5,
            libc::CS6 => 6,
            libc::CS7 => 7,
            _ => 8,
        },
        parity: match (flags & libc::PARENB != 0, flags & libc::PARODD != 0) {
            (false, _) => Parity::None,
            (true, true) => Parity::Odd,
            (true, false) => Parity::Even,
        },
        stop_bits: if flags & libc::CSTOPB != 0 { 2 } else { 1 },
    })
}

/// macOS stores the rate itself; Linux stores a B* code.
#[cfg(not(target_os = "linux"))]
fn baud(speed: libc::speed_t) -> Option<u32> {
    u32::try_from(speed).ok().filter(|&b| b > 0)
}

#[cfg(target_os = "linux")]
fn baud(speed: libc::speed_t) -> Option<u32> {
    use libc::*;
    Some(match speed {
        B300 => 300,
        B600 => 600,
        B1200 => 1200,
        B2400 => 2400,
        B4800 => 4800,
        B9600 => 9600,
        B19200 => 19200,
        B38400 => 38400,
        B57600 => 57600,
        B115200 => 115_200,
        B230400 => 230_400,
        B460800 => 460_800,
        B500000 => 500_000,
        B921600 => 921_600,
        B1000000 => 1_000_000,
        B2000000 => 2_000_000,
        _ => return None,
    })
}

fn readable(f: &File, timeout: Duration) -> bool {
    let mut p = libc::pollfd {
        fd: f.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: p is one valid pollfd.
    let n = unsafe { libc::poll(&mut p, 1, timeout.as_millis() as libc::c_int) };
    n > 0
}
