mod config;
mod discovery;
mod serial;

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::{net::TcpListener, sync::Notify};
use tokio_util::codec::Framed;
use tracing::{info, warn};
use usbridge_proto::{
    ClientMsg, ErrorCode, FrameCodec, Profile, ServerMsg, DEFAULT_PORT, PROTOCOL_VERSION,
};

use config::Config;

#[derive(Parser)]
#[command(name = "usbridged", about = "Export USB serial devices over the network")]
struct Args {
    #[arg(short, long, env = "USBRIDGE_CONFIG", default_value = "/etc/usbridge/config.toml")]
    config: PathBuf,

    /// Override the listen address from the config file.
    #[arg(short, long)]
    listen: Option<String>,
}

/// Which client currently holds each device. An eviction `Notify` lets a later
/// client with `steal` take over without waiting for the holder to time out.
#[derive(Default)]
struct Registry {
    held: Mutex<HashMap<String, Arc<Notify>>>,
}

impl Registry {
    fn is_busy(&self, id: &str) -> bool {
        self.held.lock().unwrap().contains_key(id)
    }

    fn claim(&self, id: &str, steal: bool) -> Option<Arc<Notify>> {
        let mut held = self.held.lock().unwrap();
        if let Some(existing) = held.get(id) {
            if !steal {
                return None;
            }
            existing.notify_waiters();
        }
        let token = Arc::new(Notify::new());
        held.insert(id.to_string(), token.clone());
        Some(token)
    }

    fn release(&self, id: &str, token: &Arc<Notify>) {
        let mut held = self.held.lock().unwrap();
        // Only clear the slot if we still own it; a stealer may have replaced us.
        if held.get(id).is_some_and(|cur| Arc::ptr_eq(cur, token)) {
            held.remove(id);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let cfg = Arc::new(config::load(&args.config)?);
    let listen = args.listen.unwrap_or_else(|| cfg.listen.clone());

    let listener = TcpListener::bind(&listen).await?;
    info!(
        addr = %listener.local_addr()?,
        devices = cfg.device.len(),
        "usbridged listening"
    );
    if cfg.device.is_empty() {
        warn!("no devices configured; clients will see an empty list");
    }

    let registry = Arc::new(Registry::default());

    loop {
        let (sock, peer) = listener.accept().await?;
        sock.set_nodelay(true).ok();

        let cfg = cfg.clone();
        let registry = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, peer, cfg, registry).await {
                warn!(%peer, error = %e, "session ended with error");
            }
        });
    }
}

async fn handle(
    sock: tokio::net::TcpStream,
    peer: SocketAddr,
    cfg: Arc<Config>,
    registry: Arc<Registry>,
) -> Result<()> {
    let mut conn: serial::Conn = Framed::new(sock, FrameCodec::new());

    loop {
        let Some(msg) = conn.next().await else {
            return Ok(());
        };
        match msg? {
            ClientMsg::Hello {
                version,
                client_name,
            } => {
                if version != PROTOCOL_VERSION {
                    conn.send(ServerMsg::Error {
                        code: ErrorCode::VersionMismatch,
                        detail: format!("server speaks v{PROTOCOL_VERSION}, client sent v{version}"),
                    })
                    .await?;
                    return Ok(());
                }
                info!(%peer, client = %client_name, "hello");
                conn.send(ServerMsg::Welcome {
                    version: PROTOCOL_VERSION,
                    server_name: cfg.name.clone(),
                })
                .await?;
            }

            ClientMsg::List => {
                let devices = cfg
                    .device
                    .iter()
                    .map(|(id, dev)| {
                        let found = discovery::resolve(dev).ok().flatten();
                        discovery::describe(id, dev, found.as_ref(), registry.is_busy(id))
                    })
                    .collect();
                conn.send(ServerMsg::Devices(devices)).await?;
            }

            ClientMsg::Ping => conn.send(ServerMsg::Pong).await?,

            ClientMsg::Attach {
                id,
                profile,
                params,
                steal,
            } => {
                let Some(dev) = cfg.device.get(&id) else {
                    conn.send(ServerMsg::Error {
                        code: ErrorCode::NoSuchDevice,
                        detail: format!("{id:?} is not configured on this server"),
                    })
                    .await?;
                    continue;
                };
                if profile != Profile::Serial || dev.profile() != Profile::Serial {
                    conn.send(ServerMsg::Error {
                        code: ErrorCode::ProfileMismatch,
                        detail: "only the serial profile is implemented".into(),
                    })
                    .await?;
                    continue;
                }
                let Some(token) = registry.claim(&id, steal) else {
                    conn.send(ServerMsg::Error {
                        code: ErrorCode::DeviceBusy,
                        detail: format!("{id:?} is held by another client; retry with --steal"),
                    })
                    .await?;
                    continue;
                };

                // Server config is authoritative for line settings unless the
                // client explicitly asked for something other than the default.
                let effective = if params == Default::default() {
                    dev.params()
                } else {
                    params
                };

                conn.send(ServerMsg::Attached {
                    id: id.clone(),
                    params: effective,
                })
                .await?;

                let result = serial::serve(&id, dev, &mut conn, effective, &token).await;
                registry.release(&id, &token);
                info!(%peer, device = %id, "detached");
                return result;
            }

            other => {
                conn.send(ServerMsg::Error {
                    code: ErrorCode::Protocol,
                    detail: format!("unexpected in control mode: {other:?}"),
                })
                .await?;
            }
        }
    }
}

const _: () = assert!(DEFAULT_PORT == 7070);
