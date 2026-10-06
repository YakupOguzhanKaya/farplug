mod pty;

use std::{path::PathBuf, time::Duration};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;
use tracing::{info, warn};
use usbridge_proto::{
    ClientMsg, FrameCodec, Profile, SerialParams, ServerMsg, DEFAULT_PORT, PROTOCOL_VERSION,
};

type Conn = Framed<TcpStream, FrameCodec<ServerMsg, ClientMsg>>;

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

#[derive(Parser)]
#[command(name = "usbridge", about = "Attach a remote USB serial device as a local pty")]
struct Args {
    #[arg(short, long, env = "USBRIDGE_SERVER", default_value = "usbgw.local")]
    server: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show what the server exports.
    List,
    /// Attach a device and expose it as a local pty.
    Attach {
        id: String,
        /// Stable path to symlink at the pty, e.g. ~/.usbridge/dev/zigbee
        #[arg(short, long)]
        link: Option<PathBuf>,
        /// Take the device even if another client holds it.
        #[arg(long)]
        steal: bool,
        /// Override the server's configured baud rate.
        #[arg(short, long)]
        baud: Option<u32>,
        /// Exit on disconnect instead of retrying forever.
        #[arg(long)]
        once: bool,
    },
}

fn with_port(s: &str) -> String {
    if s.contains(':') {
        s.to_string()
    } else {
        format!("{s}:{DEFAULT_PORT}")
    }
}

async fn connect(server: &str) -> Result<Conn> {
    let sock = TcpStream::connect(with_port(server)).await?;
    sock.set_nodelay(true).ok();
    let mut conn: Conn = Framed::new(sock, FrameCodec::new());

    conn.send(ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        client_name: hostname(),
    })
    .await?;

    match conn.next().await {
        Some(Ok(ServerMsg::Welcome { server_name, .. })) => {
            info!(server = %server_name, "connected");
            Ok(conn)
        }
        Some(Ok(ServerMsg::Error { code, detail })) => bail!("server refused: {code:?}: {detail}"),
        Some(Ok(other)) => bail!("unexpected greeting: {other:?}"),
        Some(Err(e)) => Err(e.into()),
        None => bail!("server closed during handshake"),
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            nix_hostname()
        })
        .unwrap_or_else(|| "usbridge-client".into())
}

fn nix_hostname() -> Option<String> {
    nix::unistd::gethostname()
        .ok()
        .and_then(|s| s.into_string().ok())
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

    match args.cmd {
        Cmd::List => list(&args.server).await,
        Cmd::Attach {
            id,
            link,
            steal,
            baud,
            once,
        } => {
            let params = SerialParams {
                baud: baud.unwrap_or(SerialParams::default().baud),
                ..Default::default()
            };

            // The pty outlives individual TCP sessions so that an application
            // holding the port survives a server restart or a network blip.
            let mut tty = pty::Pty::open()?;
            if let Some(link) = &link {
                tty.link_at(link)?;
            }
            println!("{}", link.as_deref().unwrap_or(&tty.path).display());

            loop {
                match session(&args.server, &id, &tty, params, steal).await {
                    Ok(()) => info!("session ended"),
                    Err(e) => warn!(error = %e, "session failed"),
                }
                if once {
                    return Ok(());
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

async fn list(server: &str) -> Result<()> {
    let mut conn = connect(server).await?;
    conn.send(ClientMsg::List).await?;

    match conn.next().await {
        Some(Ok(ServerMsg::Devices(devices))) => {
            if devices.is_empty() {
                println!("no devices configured");
                return Ok(());
            }
            println!("{:<16} {:<28} {:<10} {}", "ID", "LABEL", "STATE", "USB");
            for d in devices {
                let state = match (d.present, d.busy) {
                    (false, _) => "absent",
                    (true, true) => "in use",
                    (true, false) => "free",
                };
                let usb = match (d.vid, d.pid) {
                    (Some(v), Some(p)) => {
                        let serial = d.serial.unwrap_or_default();
                        format!("{v:04x}:{p:04x} {serial}")
                    }
                    _ => "-".into(),
                };
                println!("{:<16} {:<28} {:<10} {}", d.id, d.label, state, usb);
            }
            Ok(())
        }
        Some(Ok(other)) => bail!("unexpected reply: {other:?}"),
        Some(Err(e)) => Err(e.into()),
        None => bail!("server closed"),
    }
}

async fn session(
    server: &str,
    id: &str,
    tty: &pty::Pty,
    params: SerialParams,
    steal: bool,
) -> Result<()> {
    let mut conn = connect(server).await?;
    conn.send(ClientMsg::Attach {
        id: id.to_string(),
        profile: Profile::Serial,
        params,
        steal,
    })
    .await?;

    match conn.next().await {
        Some(Ok(ServerMsg::Attached { params, .. })) => {
            info!(device = id, baud = params.baud, "attached");
        }
        Some(Ok(ServerMsg::Error { code, detail })) => bail!("attach refused: {code:?}: {detail}"),
        Some(Ok(other)) => bail!("unexpected reply: {other:?}"),
        Some(Err(e)) => return Err(e.into()),
        None => bail!("server closed"),
    }

    let mut buf = vec![0u8; 8192];
    loop {
        tokio::select! {
            read = tty.read(&mut buf) => match read {
                Ok(0) => return Ok(()),
                Ok(n) => conn.send(ClientMsg::Data(buf[..n].to_vec())).await?,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            },

            msg = conn.next() => match msg {
                None => return Ok(()),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(ServerMsg::Data(bytes))) => tty.write_all(&bytes).await?,
                Some(Ok(ServerMsg::DeviceGone)) => warn!(device = id, "device unplugged; holding pty"),
                Some(Ok(ServerMsg::DeviceBack)) => info!(device = id, "device back"),
                Some(Ok(ServerMsg::Error { code, detail })) => bail!("{code:?}: {detail}"),
                Some(Ok(_)) => {}
            },
        }
    }
}
