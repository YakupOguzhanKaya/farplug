use std::sync::Arc;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use anyhow::bail;
use anyhow::Result;

use crate::Server;

/// The OS facility that makes a remote device appear local.
pub trait Backend: Send + Sync {
    /// Import `id` from the server and plug it in locally.
    fn attach(&self, server: &Server, id: &str) -> Result<Attached>;
    fn is_attached(&self, a: &Attached, id: &str) -> bool;
    fn detach(&self, a: &Attached) -> Result<()>;
}

pub struct Attached {
    pub port: u32,
    /// Only meaningful on Linux, where it tells our device from a later one
    /// on the same port.
    pub devid: u32,
}

/// `FARPLUG_BACKEND=user` selects the userspace backend that macOS uses, so
/// it can be tested on Linux.
#[cfg(target_os = "linux")]
pub fn platform() -> Result<Arc<dyn Backend>> {
    if std::env::var_os("FARPLUG_BACKEND").is_some_and(|v| v == "user") {
        return Ok(Arc::new(crate::user::UserSpace::new()));
    }
    Ok(Arc::new(crate::linux::VhciHcd::new()?))
}

#[cfg(target_os = "macos")]
pub fn platform() -> Result<Arc<dyn Backend>> {
    Ok(Arc::new(crate::user::UserSpace::new()))
}

#[cfg(windows)]
pub fn platform() -> Result<Arc<dyn Backend>> {
    Ok(Arc::new(crate::windows::UsbipWin2::new()?))
}

/// Lets the agent, GUI and CLI run everywhere; only attaching itself fails.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn platform() -> Result<Arc<dyn Backend>> {
    Ok(Arc::new(Unsupported))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
struct Unsupported;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
const UNSUPPORTED: &str = "attaching devices is not supported on this OS";

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Backend for Unsupported {
    fn attach(&self, _: &Server, _: &str) -> Result<Attached> {
        bail!(UNSUPPORTED)
    }
    fn is_attached(&self, _: &Attached, _: &str) -> bool {
        false
    }
    fn detach(&self, _: &Attached) -> Result<()> {
        Ok(())
    }
}

/// Whether this machine can attach devices: driver installed and loaded.
pub fn check() -> Result<()> {
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    bail!(UNSUPPORTED);
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    platform().map(|_| ())
}
