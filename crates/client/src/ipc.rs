//! How the CLI and GUI talk to the agent: one JSON line each way over a
//! loopback TCP connection. The agent publishes its port and a random token
//! in a file only permitted users can read, so reading that file is the
//! permission check.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::Paths;

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LINE: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// Attach and remember: they are re-attached whenever the agent starts.
    Attach {
        server: String,
        devices: Vec<String>,
    },
    /// Detach and forget. Without a server, matches the id on every server.
    Detach {
        server: Option<String>,
        devices: Vec<String>,
    },
    AddServer {
        server: String,
    },
    /// Also detaches every device attached from it.
    RemoveServer {
        server: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Normalized `host:port`, in the order they were added.
    pub servers: Vec<String>,
    pub devices: Vec<DeviceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub server: String,
    pub id: String,
    pub state: DeviceState,
    pub port: Option<u32>,
    /// Why the last attempt failed, while retrying.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    Connecting,
    Attached,
    Retrying,
}

impl std::fmt::Display for DeviceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Connecting => "connecting",
            Self::Attached => "attached",
            Self::Retrying => "retrying",
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    token: String,
    #[serde(flatten)]
    request: Request,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reply {
    Ok(Status),
    Error(String),
}

/// Send one request to the running agent.
pub fn call(paths: &Paths, request: Request) -> Result<Status> {
    let ep = read_endpoint(paths)?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, ep.port));
    let mut sock = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| {
        anyhow!("the farplug agent is not running ({e}); start it with `farplug agent`")
    })?;
    sock.set_read_timeout(Some(TIMEOUT))?;
    sock.set_write_timeout(Some(TIMEOUT))?;
    write_line(
        &mut sock,
        &Envelope {
            token: ep.token,
            request,
        },
    )?;
    match read_line::<Reply>(&mut sock).context("reading the agent's reply")? {
        Reply::Ok(status) => Ok(status),
        Reply::Error(e) => Err(anyhow!(e)),
    }
}

fn read_endpoint(paths: &Paths) -> Result<Endpoint> {
    let p = &paths.endpoint;
    let text = match fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            bail!("the farplug agent is not running; start it with `farplug agent`")
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => bail!(
            "no permission to read {}; add yourself to the `farplug` group \
             (sudo usermod -aG farplug $USER) and log in again",
            p.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

/// Agent side: read a request and check its token.
pub fn read_request(sock: &mut TcpStream, token: &str) -> Result<Request> {
    let env: Envelope = read_line(sock)?;
    if env.token != token {
        bail!("bad token");
    }
    Ok(env.request)
}

pub fn write_reply(sock: &mut TcpStream, result: Result<Status>) -> Result<()> {
    let reply = match result {
        Ok(s) => Reply::Ok(s),
        Err(e) => Reply::Error(format!("{e:#}")),
    };
    write_line(sock, &reply)
}

fn write_line<T: Serialize>(sock: &mut TcpStream, v: &T) -> Result<()> {
    let mut line = serde_json::to_vec(v)?;
    line.push(b'\n');
    sock.write_all(&line)?;
    Ok(())
}

fn read_line<T: for<'de> Deserialize<'de>>(sock: &mut TcpStream) -> Result<T> {
    let mut line = String::new();
    BufReader::new(io::Read::take(&mut *sock, MAX_LINE)).read_line(&mut line)?;
    if line.is_empty() {
        bail!("connection closed");
    }
    Ok(serde_json::from_str(&line)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format() {
        let env = Envelope {
            token: "t".into(),
            request: Request::Attach {
                server: "gw:3240".into(),
                devices: vec!["zigbee".into()],
            },
        };
        assert_eq!(
            serde_json::to_string(&env).unwrap(),
            r#"{"token":"t","op":"attach","server":"gw:3240","devices":["zigbee"]}"#
        );
        let back: Envelope = serde_json::from_str(r#"{"token":"t","op":"status"}"#).unwrap();
        assert_eq!(back.request, Request::Status);
    }
}
