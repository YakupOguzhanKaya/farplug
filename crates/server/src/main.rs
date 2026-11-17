mod add;
mod config;
mod exports;
mod sysfs;
mod table;

use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

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
    /// Pick plugged-in devices and add config entries for them.
    Add {
        /// Ports from `scan`, e.g. 1-1.4; without them, asks which.
        ports: Vec<String>,
    },
    /// Validate the config and show which device each entry would export.
    /// A running server applies config changes on its own.
    Check,
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
        Cmd::Serve { listen } => serve(&cli.config, sysfs, listen),
        Cmd::Scan => scan(&sysfs),
        Cmd::Check => check(&cli.config, &sysfs),
        Cmd::Add { ports } => add::run(
            &cli.config,
            &sysfs,
            &ports,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout(),
        ),
        Cmd::Release { devices } => release(&cli.config, &sysfs, &devices),
    }
}

fn serve(config: &Path, sysfs: Sysfs, listen: Option<String>) -> Result<()> {
    let cfg = Config::load(config)?;
    sysfs.ensure_usbip_host()?;
    if cfg.device.is_empty() {
        warn!("no [device.*] entries in the config; nothing will be exported");
    }

    let fixed_listen = listen.is_some();
    let addr = listen.unwrap_or_else(|| cfg.listen.clone());
    let listener = TcpListener::bind(&addr).with_context(|| format!("listening on {addr}"))?;
    info!(%addr, "listening");

    let exports = Arc::new(Mutex::new(Exports::new(cfg, sysfs)));
    exports.lock().unwrap().reconcile()?;

    let ex = exports.clone();
    let config = config.to_owned();
    let mut seen = modified(&config);
    thread::spawn(move || loop {
        thread::sleep(POLL);
        let now = modified(&config);
        if now != seen {
            seen = now;
            reload(&config, &ex, fixed_listen);
        }
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

fn modified(path: &Path) -> Option<(SystemTime, u64)> {
    let m = fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// A config that fails to load is ignored; the running one stays in effect.
fn reload(path: &Path, exports: &Mutex<Exports>, fixed_listen: bool) {
    let cfg = match Config::load(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("config changed but not applied, keeping the previous one: {e:#}");
            return;
        }
    };
    let mut ex = exports.lock().unwrap();
    if !fixed_listen && cfg.listen != ex.config().listen {
        warn!(listen = %cfg.listen, "the listen address changed; restart farplugd to apply it");
    }
    info!(devices = cfg.device.len(), "config changed; applying it");
    ex.set_config(cfg);
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

fn check(path: &Path, sysfs: &Sysfs) -> Result<()> {
    let cfg = Config::load(path)?;
    println!("{}: valid, {} device(s)", path.display(), cfg.device.len());
    if cfg.device.is_empty() {
        return Ok(());
    }
    let devices = match sysfs.scan() {
        Ok(d) => d,
        Err(e) => {
            println!("cannot read USB devices to check the matchers: {e:#}");
            return Ok(());
        }
    };
    let mut owners: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut rows = vec![table::row(["ID", "PORT", "USB ID", "RESULT"])];
    for (id, want) in &cfg.device {
        let found = exports::candidates(want, &devices);
        for d in &found {
            owners.entry(&d.busid).or_default().push(id);
        }
        let (port, usb, result) = match found.as_slice() {
            [] => ("-".into(), "-".into(), "not plugged in".into()),
            [d] => (
                d.busid.clone(),
                format!("{:04x}:{:04x}", d.vendor_id, d.product_id),
                format!("ok: {}", d.description()),
            ),
            many => (
                many.iter()
                    .map(|d| d.busid.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                "-".into(),
                format!("{} devices match; add a serial or port matcher", many.len()),
            ),
        };
        rows.push(vec![id.clone(), port, usb, result]);
    }
    table::print(&rows);
    for (busid, ids) in owners.iter().filter(|(_, ids)| ids.len() > 1) {
        println!(
            "warning: {busid} matches {}; only {} exports it",
            ids.join(", "),
            ids[0]
        );
    }
    Ok(())
}

fn scan(sysfs: &Sysfs) -> Result<()> {
    let devices = sysfs.scan()?;
    if devices.is_empty() {
        println!("no USB devices found");
        return Ok(());
    }
    let mut rows = vec![table::row([
        "PORT",
        "USB ID",
        "SERIAL",
        "DRIVER",
        "DESCRIPTION",
    ])];
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
        rows.push(vec![
            d.busid.clone(),
            format!("{:04x}:{:04x}", d.vendor_id, d.product_id),
            d.serial.clone().unwrap_or_else(|| "-".into()),
            driver,
            d.description(),
        ]);
    }
    table::print(&rows);
    println!("\nRun `sudo farplugd add` to pick devices to export.");
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
