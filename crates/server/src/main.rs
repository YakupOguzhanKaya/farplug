mod config;
mod exports;
mod sysfs;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use farplug_usbip::{self as usbip, Request, Status};
use tracing::{debug, info, warn};

use config::Config;
use exports::Exports;
use sysfs::{StubStatus, Sysfs};

const POLL: Duration = Duration::from_secs(1);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(version, about = "Export USB devices over the network (USB/IP server)")]
struct Cli {
    #[arg(
        short,
        long,
        env = "FARPLUG_CONFIG",
        default_value = "/etc/farplug/config.toml"
    )]
    config: PathBuf,
    /// For tests: read and write a fake sysfs tree instead of /sys.
    #[arg(long, hide = true, default_value = "/sys")]
    sysfs_root: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (the default).
    Serve {
        /// Overrides `listen` from the config file.
        #[arg(short, long)]
        listen: Option<String>,
    },
    /// List the USB devices on this machine, to help write the config.
    Scan,
    /// Give devices back to this machine's own drivers. Clients using them
    /// are disconnected; a running server re-exports them on its next poll.
    Release {
        /// Export ids from the config, or raw busids such as 1-1.4.
        #[arg(required = true)]
        devices: Vec<String>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let sysfs = Sysfs::new(&cli.sysfs_root);
    match cli.cmd.unwrap_or(Cmd::Serve { listen: None }) {
        Cmd::Serve { listen } => serve(Config::load(&cli.config)?, sysfs, listen),
        Cmd::Scan => scan(&sysfs),
        Cmd::Release { devices } => release(&cli.config, &sysfs, &devices),
    }
}

fn serve(cfg: Config, sysfs: Sysfs, listen: Option<String>) -> Result<()> {
    sysfs.ensure_usbip_host()?;
    if cfg.device.is_empty() {
        warn!("no [device.*] entries in the config; nothing will be exported");
    }

    let addr = listen.unwrap_or_else(|| cfg.listen.clone());
    let listener = TcpListener::bind(&addr).with_context(|| format!("listening on {addr}"))?;
    info!(%addr, "listening");

    let exports = Arc::new(Mutex::new(Exports::new(cfg, sysfs)));
    exports.lock().unwrap().reconcile()?;

    let ex = exports.clone();
    thread::spawn(move || loop {
        thread::sleep(POLL);
        if let Err(e) = ex.lock().unwrap().reconcile() {
            warn!(error = %e, "scanning USB devices failed");
        }
    });

    for conn in listener.incoming() {
        let sock = match conn {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "accept failed");
                continue;
            }
        };
        let peer = sock.peer_addr().ok();
        let ex = exports.clone();
        thread::spawn(move || {
            if let Err(e) = handle(sock, &ex) {
                debug!(?peer, error = %format!("{e:#}"), "connection ended with error");
            }
        });
    }
    Ok(())
}

fn handle(mut sock: TcpStream, exports: &Mutex<Exports>) -> Result<()> {
    let peer: Option<SocketAddr> = sock.peer_addr().ok();
    sock.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    sock.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;

    match Request::read_from(&mut sock)? {
        Request::DevList => {
            let list = exports.lock().unwrap().list();
            usbip::write_devlist_reply(&mut sock, &list)?;
        }
        Request::Info => {
            let info = exports.lock().unwrap().info();
            usbip::write_info_reply(&mut sock, &info)?;
        }
        Request::Import { busid: name } => {
            let mut ex = exports.lock().unwrap();
            let Some(dev) = ex.find(&name).cloned() else {
                info!(?peer, device = %name, "import refused: not exported");
                return Ok(usbip::write_import_reply(&mut sock, Err(Status::NoDevice))?);
            };
            let refusal = match ex.sysfs.stub_status(&dev.busid) {
                Ok(StubStatus::Available) => None,
                Ok(StubStatus::Used) => Some(Status::DeviceBusy),
                Ok(StubStatus::Error) => Some(Status::DeviceError),
                Err(e) => {
                    warn!(device = %name, error = %e, "reading usbip_status failed");
                    Some(Status::NotAvailable)
                }
            };
            if let Some(status) = refusal {
                info!(?peer, device = %name, "import refused: {status}");
                return Ok(usbip::write_import_reply(&mut sock, Err(status))?);
            }

            usbip::prepare_for_kernel(&sock)?;
            // Same order as usbipd: the kernel owns the socket before the
            // reply goes out, so no URB can arrive before it is listening.
            if let Err(e) = ex.sysfs.hand_over_socket(&dev.busid, sock.as_raw_fd()) {
                warn!(device = %name, error = %format!("{e:#}"), "handing socket to usbip-host failed");
                return Ok(usbip::write_import_reply(
                    &mut sock,
                    Err(Status::NotAvailable),
                )?);
            }
            if let Some(peer) = peer {
                ex.record_attach(&dev.busid, peer);
            }
            drop(ex);
            usbip::write_import_reply(&mut sock, Ok(&dev.to_usbip(&name)))?;
            info!(?peer, device = %name, busid = %dev.busid, "attached");
        }
    }
    Ok(())
}

fn scan(sysfs: &Sysfs) -> Result<()> {
    let devices = sysfs.scan()?;
    if devices.is_empty() {
        println!("no USB devices found");
        return Ok(());
    }
    println!(
        "{:<10} {:<10} {:<20} {:<16} DESCRIPTION",
        "PORT", "USB ID", "SERIAL", "DRIVER"
    );
    for d in &devices {
        let driver = match (d.driver.as_deref(), d.is_hub()) {
            (_, true) => "(hub)".to_owned(),
            (Some(sysfs::USBIP_HOST), _) => match sysfs.stub_status(&d.busid) {
                Ok(StubStatus::Used) => "exported, in use".to_owned(),
                _ => "exported".to_owned(),
            },
            (Some(drv), _) => drv.to_owned(),
            (None, _) => "-".to_owned(),
        };
        println!(
            "{:<10} {:04x}:{:04x}  {:<20} {:<16} {}",
            d.busid,
            d.vendor_id,
            d.product_id,
            d.serial.as_deref().unwrap_or("-"),
            driver,
            d.description()
        );
    }
    println!(
        "\nExport one by adding to the config, e.g.:\n\n  [device.mydevice]\n  vid = \"{:04x}\"\n  pid = \"{:04x}\"",
        devices[0].vendor_id, devices[0].product_id
    );
    if let Some(s) = &devices[0].serial {
        println!("  serial = \"{s}\"");
    }
    println!("\nUse `port = \"<PORT>\"` instead when identical devices have no serial.");
    Ok(())
}

fn release(config: &std::path::Path, sysfs: &Sysfs, names: &[String]) -> Result<()> {
    let cfg = Config::load(config).ok();
    let devices = sysfs.scan()?;
    for name in names {
        let dev = match cfg.as_ref().and_then(|c| c.device.get(name)) {
            Some(want) => devices
                .iter()
                .find(|d| want.matches(d) && d.is_bound_to_usbip()),
            None => devices.iter().find(|d| d.busid == *name),
        };
        let Some(dev) = dev else {
            bail!("{name:?} is not a configured id or a busid that is plugged in");
        };
        sysfs
            .release(&dev.busid)
            .with_context(|| format!("releasing {}", dev.busid))?;
        println!("{name}: {} returned to the host", dev.busid);
    }
    Ok(())
}
