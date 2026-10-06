use std::time::Duration;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};
use usbridge_proto::{
    ClientMsg, ControlLines, ErrorCode, FlowControl, FrameCodec, Parity, SerialParams, ServerMsg,
};

use crate::config::DeviceCfg;
use crate::discovery;

pub type Conn = Framed<tokio::net::TcpStream, FrameCodec<ClientMsg, ServerMsg>>;

const READ_BUF: usize = 8192;
const REOPEN_INTERVAL: Duration = Duration::from_secs(1);

pub fn open(path: &str, p: &SerialParams) -> Result<SerialStream> {
    let builder = tokio_serial::new(path, p.baud)
        .data_bits(match p.data_bits {
            5 => tokio_serial::DataBits::Five,
            6 => tokio_serial::DataBits::Six,
            7 => tokio_serial::DataBits::Seven,
            _ => tokio_serial::DataBits::Eight,
        })
        .stop_bits(match p.stop_bits {
            2 => tokio_serial::StopBits::Two,
            _ => tokio_serial::StopBits::One,
        })
        .parity(match p.parity {
            Parity::None => tokio_serial::Parity::None,
            Parity::Odd => tokio_serial::Parity::Odd,
            Parity::Even => tokio_serial::Parity::Even,
        })
        .flow_control(match p.flow_control {
            FlowControl::None => tokio_serial::FlowControl::None,
            FlowControl::Software => tokio_serial::FlowControl::Software,
            FlowControl::Hardware => tokio_serial::FlowControl::Hardware,
        })
        .timeout(Duration::from_millis(50));

    let mut port = builder
        .open_native_async()
        .with_context(|| format!("opening {path}"))?;

    // Without this the kernel line discipline echoes and cooks the stream on
    // some platforms, which corrupts binary protocols like Zigbee EZSP.
    #[cfg(unix)]
    port.set_exclusive(true).ok();

    Ok(port)
}

/// Serve one attached client until it disconnects, the device is stolen, or the
/// connection errors. Physical unplugs are survived, not fatal.
pub async fn serve(
    id: &str,
    cfg: &DeviceCfg,
    conn: &mut Conn,
    mut params: SerialParams,
    evict: &Notify,
) -> Result<()> {
    let mut gone = false;

    loop {
        let found = discovery::resolve(cfg).ok().flatten();
        let opened = found.as_ref().and_then(|f| match open(&f.path, &params) {
            Ok(port) => Some((f.path.clone(), port)),
            Err(e) => {
                warn!(device = id, error = format!("{e:#}"), "open failed");
                None
            }
        });

        let Some((path, mut port)) = opened else {
            if !gone {
                gone = true;
                warn!(device = id, "device absent, waiting");
                conn.send(ServerMsg::DeviceGone).await?;
            }
            if wait_while_absent(conn, evict).await? {
                return Ok(());
            }
            continue;
        };

        if gone {
            gone = false;
            info!(device = id, %path, "device back");
            conn.send(ServerMsg::DeviceBack).await?;
        } else {
            info!(device = id, %path, baud = params.baud, "attached");
        }

        match pump(&mut port, conn, &mut params, evict).await? {
            Outcome::ClientDone | Outcome::Evicted => return Ok(()),
            Outcome::PortLost => continue,
        }
    }
}

enum Outcome {
    ClientDone,
    Evicted,
    PortLost,
}

async fn pump(
    port: &mut SerialStream,
    conn: &mut Conn,
    params: &mut SerialParams,
    evict: &Notify,
) -> Result<Outcome> {
    let mut buf = vec![0u8; READ_BUF];

    loop {
        tokio::select! {
            biased;

            _ = evict.notified() => return Ok(Outcome::Evicted),

            read = port.read(&mut buf) => match read {
                Ok(0) => return Ok(Outcome::PortLost),
                Ok(n) => conn.send(ServerMsg::Data(buf[..n].to_vec())).await?,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => {
                    warn!(error = %e, "serial read failed");
                    return Ok(Outcome::PortLost);
                }
            },

            msg = conn.next() => {
                let Some(msg) = msg else { return Ok(Outcome::ClientDone) };
                match msg? {
                    ClientMsg::Data(bytes) => {
                        if let Err(e) = port.write_all(&bytes).await {
                            warn!(error = %e, "serial write failed");
                            return Ok(Outcome::PortLost);
                        }
                    }
                    ClientMsg::SetControlLines(ControlLines { dtr, rts }) => {
                        debug!(dtr, rts, "control lines");
                        let _ = port.write_data_terminal_ready(dtr);
                        let _ = port.write_request_to_send(rts);
                    }
                    ClientMsg::SetParams(p) => {
                        *params = p;
                        if port.set_baud_rate(p.baud).is_err() {
                            // Some drivers only apply a rate change on reopen.
                            return Ok(Outcome::PortLost);
                        }
                    }
                    ClientMsg::Break { millis } => {
                        if port.set_break().is_ok() {
                            tokio::time::sleep(Duration::from_millis(millis as u64)).await;
                            let _ = port.clear_break();
                        }
                    }
                    ClientMsg::Ping => conn.send(ServerMsg::Pong).await?,
                    other => {
                        conn.send(ServerMsg::Error {
                            code: ErrorCode::Protocol,
                            detail: format!("unexpected in data mode: {other:?}"),
                        })
                        .await?;
                    }
                }
            }
        }
    }
}

/// Keep the control channel responsive while the hardware is missing.
/// Returns true if the session should end.
async fn wait_while_absent(conn: &mut Conn, evict: &Notify) -> Result<bool> {
    tokio::select! {
        biased;
        _ = evict.notified() => Ok(true),
        _ = tokio::time::sleep(REOPEN_INTERVAL) => Ok(false),
        msg = conn.next() => match msg {
            None => Ok(true),
            Some(Err(e)) => Err(e.into()),
            // Writes are dropped while absent; the client is told via DeviceGone.
            Some(Ok(ClientMsg::Ping)) => {
                conn.send(ServerMsg::Pong).await?;
                Ok(false)
            }
            Some(Ok(_)) => Ok(false),
        },
    }
}
