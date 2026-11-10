//! Linux backend: we do the import handshake, then hand the socket to vhci_hcd.

use std::os::fd::AsRawFd;

use anyhow::Result;
use usbridge_usbip::{self as usbip, Request};

use crate::vhci::Vhci;
use crate::{Attached, Backend, Server};

pub struct VhciHcd(Vhci);

impl VhciHcd {
    pub fn new() -> Result<Self> {
        let vhci = Vhci::default();
        vhci.ensure_loaded()?;
        Ok(Self(vhci))
    }
}

impl Backend for VhciHcd {
    fn attach(&self, server: &Server, id: &str) -> Result<Attached> {
        let mut sock = server.connect()?;
        Request::Import {
            busid: id.to_owned(),
        }
        .write_to(&mut sock)?;
        let dev = usbip::read_import_reply(&mut sock)?;
        usbip::prepare_for_kernel(&sock)?;
        let port = self.0.attach(&dev, sock.as_raw_fd())?;
        // vhci_hcd holds its own reference to the socket now.
        drop(sock);
        Ok(Attached {
            port,
            devid: dev.devid(),
        })
    }

    fn is_attached(&self, a: &Attached, _id: &str) -> bool {
        self.0
            .ports()
            .map(|ports| ports.iter().any(|p| p.port == a.port && p.holds(a.devid)))
            .unwrap_or(false)
    }

    fn detach(&self, a: &Attached) -> Result<()> {
        self.0.detach(a.port)
    }
}
