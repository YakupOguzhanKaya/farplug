use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use farplug::ipc::{self, Request, Status};
use farplug::paths::Paths;
use farplug::{agent, backend, supervisor, Server};

#[derive(Parser)]
#[command(version, about = "Use USB devices exported by a farplugd server")]
struct Cli {
    /// Server address, host or host:port.
    #[arg(short, long, global = true, env = "FARPLUG_SERVER")]
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
            None => bail!("which server? pass --server or set FARPLUG_SERVER"),
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
    let mut rows = vec![row(["ID", "USB ID", "SPEED", "INTERFACE CLASSES"])];
    for d in devices {
        let classes: Vec<String> = d
            .interfaces
            .iter()
            .map(|i| format!("{:02x}", i.class))
            .collect();
        rows.push(vec![
            d.device.busid,
            format!("{:04x}:{:04x}", d.device.vendor_id, d.device.product_id),
            d.device.speed.to_string(),
            classes.join(" "),
        ]);
    }
    print_table(&rows);
    Ok(())
}

fn print_status(s: &Status) -> Result<()> {
    if s.devices.is_empty() {
        println!("no devices attached");
        return Ok(());
    }
    let mut rows = vec![row(["SERVER", "DEVICE", "STATE", "PORT", "NOTE"])];
    for d in &s.devices {
        rows.push(vec![
            d.server.clone(),
            d.id.clone(),
            d.state.to_string(),
            d.port.map_or("-".into(), |p| p.to_string()),
            d.error.clone().unwrap_or_default(),
        ]);
    }
    print_table(&rows);
    Ok(())
}

fn row<const N: usize>(cells: [&str; N]) -> Vec<String> {
    cells.map(str::to_owned).to_vec()
}

/// Left-aligned columns sized to their widest cell; the last column is not
/// padded.
fn print_table(rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for r in rows {
        let mut line = String::new();
        for (c, cell) in r.iter().enumerate() {
            line.push_str(cell);
            if c + 1 < r.len() {
                line.push_str(&" ".repeat(widths[c] - cell.chars().count() + 2));
            }
        }
        println!("{}", line.trim_end());
    }
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
