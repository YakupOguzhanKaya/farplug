//! The backend for systems without a kernel USB/IP client (macOS): farplug
//! drives each device itself and offers it as a local serial port.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use tracing::info;

use crate::backend::{Attached, Backend};
use crate::serial::Port;
use crate::userhost::Device;
use crate::Server;

pub struct UserSpace {
    dir: PathBuf,
    ports: Mutex<HashMap<u32, Port>>,
    next: AtomicU32,
}

impl UserSpace {
    pub fn new() -> Self {
        Self {
            dir: crate::paths::serial_dir(),
            ports: Mutex::new(HashMap::new()),
            next: AtomicU32::new(1),
        }
    }
}

impl Backend for UserSpace {
    fn attach(&self, server: &Server, id: &str) -> Result<Attached> {
        let dev = Device::open(server, id)?;
        let devid = dev.devid;
        let port = Port::start(dev, &self.dir.join(id))?;
        info!(
            device = id,
            chip = port.chip.name(),
            link = %port.link().display(),
            tty = %port.tty,
            "serial port ready"
        );
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        self.ports.lock().unwrap().insert(n, port);
        Ok(Attached { port: n, devid })
    }

    fn is_attached(&self, a: &Attached, _: &str) -> bool {
        let mut ports = self.ports.lock().unwrap();
        match ports.get(&a.port) {
            Some(p) if p.is_alive() => true,
            Some(_) => {
                // Dropping it stops its threads and removes its link.
                ports.remove(&a.port);
                false
            }
            None => false,
        }
    }

    fn detach(&self, a: &Attached) -> Result<()> {
        self.ports.lock().unwrap().remove(&a.port);
        Ok(())
    }
}
