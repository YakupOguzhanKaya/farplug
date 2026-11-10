use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use usbridge::ipc::{self, Request, Status};
use usbridge::paths::Paths;
use usbridge::{agent, backend, supervisor, Server};

#[derive(Parser)]
#[command(version, about = "Use USB devices exported by a usbridged server")]
struct Cli {
    /// Server address, host or host:port.
    #[arg(short, long, global = true, env = "USBRIDGE_SERVER")]
    server: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the devices a server exports.
    List,
    /// Attach devices through the agent and keep them attached, including
    /// after a reboot, until detached.
    Attach {
        #[arg(required = true)]
        devices: Vec<String>,
        /// Attach in this process instead of through the agent, until Ctrl-C.
        #[arg(long)]
        foreground: bool,
    },
    /// Detach devices and stop re-attaching them.
    Detach {
        #[arg(required_unless_present = "all")]
        devices: Vec<String>,
        /// Detach every device the agent holds.
        #[arg(long, conflicts_with = "devices")]
        all: bool,
    },
    /// Check that this OS can attach devices (driver installed and loaded).
    Check,
    /// Show what the agent is keeping attached.
    Status,
    /// Run the agent that owns attachments (normally started as a service).
    Agent,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let paths = Paths::default();
    let server = || -> Result<Server> {
        match &cli.server {
            Some(s) => Server::parse(s),
            None => bail!("which server? pass --server or set USBRIDGE_SERVER"),
        }
    };
    match cli.cmd {
        Cmd::List => list(&server()?),
        Cmd::Attach {
            devices,
            foreground: true,
        } => foreground(server()?, devices),
        Cmd::Attach { devices, .. } => print_status(&ipc::call(
            &paths,
            Request::Attach {
                server: server()?.to_string(),
                devices,
            },
        )?),
        Cmd::Detach { devices, all } => {
            let devices = if all {
                let mut ids: Vec<String> = ipc::call(&paths, Request::Status)?
                    .devices
                    .into_iter()
                    .map(|d| d.id)
                    .collect();
                ids.sort();
                ids.dedup();
                if ids.is_empty() {
                    return print_status(&Status::default());
                }
                ids
            } else {
                devices
            };
            print_status(&ipc::call(
                &paths,
                Request::Detach {
                    server: if all { None } else { cli.server.clone() },
                    devices,
                },
            )?)
        }
        Cmd::Check => {
            backend::check()?;
            println!("ok: this machine can attach USB/IP devices");
            Ok(())
        }
        Cmd::Status => print_status(&ipc::call(&paths, Request::Status)?),
        Cmd::Agent => agent::run(paths),
    }
}

fn list(server: &Server) -> Result<()> {
    let devices = server.devices()?;
    if devices.is_empty() {
        println!("{server} exports no devices right now");
        return Ok(());
    }
    println!(
        "{:<20} {:<10} {:<10} INTERFACE CLASSES",
        "ID", "USB ID", "SPEED"
    );
    for d in devices {
        let classes: Vec<String> = d
            .interfaces
            .iter()
            .map(|i| format!("{:02x}", i.class))
            .collect();
        println!(
            "{:<20} {:04x}:{:04x}  {:<10} {}",
            d.device.busid,
            d.device.vendor_id,
            d.device.product_id,
            d.device.speed.to_string(),
            classes.join(" ")
        );
    }
    Ok(())
}

fn print_status(s: &Status) -> Result<()> {
    if s.devices.is_empty() {
        println!("no devices attached");
        return Ok(());
    }
    println!(
        "{:<24} {:<20} {:<11} {:<5} NOTE",
        "SERVER", "DEVICE", "STATE", "PORT"
    );
    for d in &s.devices {
        println!(
            "{:<24} {:<20} {:<11} {:<5} {}",
            d.server,
            d.id,
            d.state.to_string(),
            d.port.map_or("-".into(), |p| p.to_string()),
            d.error.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn foreground(server: Server, ids: Vec<String>) -> Result<()> {
    let backend = backend::platform()?;
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::Relaxed))
        .context("installing the Ctrl-C handler")?;

    let workers: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let (server, backend, stop) = (server.clone(), backend.clone(), stop.clone());
            thread::spawn(move || {
                supervisor::keep_attached(&server, &id, &*backend, &stop, &|_, _, _| {})
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}
