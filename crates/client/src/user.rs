//! The backend for systems without a kernel USB/IP client (macOS): farplug
//! drives each device itself. Serial adapters become local serial ports;
//! keyboards and mice are replayed as local input.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Result};
use tracing::info;

use crate::backend::{Attached, Backend};
use crate::hid;
use crate::serial::{chips::Chip, Port};
use crate::userhost::Device;
use crate::Server;

enum Session {
    Serial(Port),
    Input(hid::Session),
}

impl Session {
    fn is_alive(&self) -> bool {
        match self {
            Self::Serial(p) => p.is_alive(),
            Self::Input(s) => s.is_alive(),
        }
    }
}

pub struct UserSpace {
    dir: PathBuf,
    sessions: Mutex<HashMap<u32, Session>>,
    next: AtomicU32,
}

impl UserSpace {
    pub fn new() -> Self {
        Self {
            dir: crate::paths::serial_dir(),
            sessions: Mutex::new(HashMap::new()),
            next: AtomicU32::new(1),
        }
    }
}

impl Backend for UserSpace {
    fn attach(&self, server: &Server, id: &str) -> Result<Attached> {
        let dev = Device::open(server, id)?;
        let devid = dev.devid;
        let (session, local) = if Chip::detect(&dev).is_some() {
            let port = Port::start(dev, &self.dir.join(id))?;
            info!(
                device = id,
                chip = port.chip.name(),
                link = %port.link().display(),
                tty = %port.tty,
                "serial port ready"
            );
            let local = format!("serial port {}", port.link().display());
            (Session::Serial(port), local)
        } else if !hid::boot_interfaces(&dev).is_empty() {
            let s = hid::Session::start(dev, Box::new(crate::inputbus::Broadcast))?;
            info!(device = id, kinds = %s.kinds, "forwarding input");
            let local = format!("{} input, typed by the farplug menu-bar app", s.kinds);
            (Session::Input(s), local)
        } else {
            bail!(
                "{:04x}:{:04x} ({}) is not a serial adapter, keyboard or mouse, \
                 the device types farplug can use on this OS",
                dev.descriptor.vendor_id,
                dev.descriptor.product_id,
                dev.product.as_deref().unwrap_or("unknown device"),
            );
        };
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        self.sessions.lock().unwrap().insert(n, session);
        Ok(Attached {
            port: n,
            devid,
            local: Some(local),
        })
    }

    fn is_attached(&self, a: &Attached, _: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(&a.port) {
            Some(s) if s.is_alive() => true,
            Some(_) => {
                // Dropping it stops its threads and removes any link.
                sessions.remove(&a.port);
                false
            }
            None => false,
        }
    }

    fn detach(&self, a: &Attached) -> Result<()> {
        self.sessions.lock().unwrap().remove(&a.port);
        Ok(())
    }
}
