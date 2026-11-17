use std::fmt;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use farplug_usbip::{self as usbip, ExportedDevice, Request, DEFAULT_PORT};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(target_os = "linux")]
const MDNS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Server {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

impl Server {
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(Self {
                host: addr.ip().to_string(),
                port: addr.port(),
            });
        }
        let (host, port) = match s.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (
                host,
                port.parse()
                    .with_context(|| format!("{port:?} is not a port number"))?,
            ),
            _ => (s.trim_matches(|c| c == '[' || c == ']'), DEFAULT_PORT),
        };
        if host.is_empty() {
            bail!("no host in {s:?}");
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    /// Resolved on every connect, so a server whose address changes is
    /// still found on the next reconnect.
    pub fn connect(&self) -> Result<TcpStream> {
        let addrs = self.resolve()?;
        let mut last = None;
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
                Ok(s) => {
                    s.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
                    s.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
                    return Ok(s);
                }
                Err(e) => last = Some((addr, e)),
            }
        }
        match last {
            Some((addr, e)) => Err(e).with_context(|| format!("connecting to {addr}")),
            None => bail!("{} did not resolve to any address", self.host),
        }
    }

    fn resolve(&self) -> Result<Vec<SocketAddr>> {
        #[cfg(target_os = "linux")]
        if crate::mdns::is_mdns_name(&self.host) {
            return match crate::mdns::resolve(&self.host, self.port, MDNS_TIMEOUT) {
                Ok(addrs) => Ok(addrs),
                // /etc/hosts or a DNS server may still know the name.
                Err(e) => self
                    .resolve_system()
                    .map_err(|_| e.context(format!("resolving {}", self.host))),
            };
        }
        self.resolve_system()
    }

    fn resolve_system(&self) -> Result<Vec<SocketAddr>> {
        Ok((self.host.as_str(), self.port)
            .to_socket_addrs()
            .with_context(|| format!("resolving {}", self.host))?
            .collect())
    }

    /// What the server exports right now.
    pub fn devices(&self) -> Result<Vec<ExportedDevice>> {
        let mut sock = self.connect()?;
        Request::DevList.write_to(&mut sock)?;
        Ok(usbip::read_devlist_reply(&mut sock)?)
    }
}

#[cfg(test)]
mod tests {
    use super::Server;

    fn p(s: &str) -> (String, u16) {
        let s = Server::parse(s).unwrap();
        (s.host, s.port)
    }

    #[test]
    fn parses_server_addresses() {
        assert_eq!(p("usbgw.local"), ("usbgw.local".into(), 3240));
        assert_eq!(p("usbgw.local:4000"), ("usbgw.local".into(), 4000));
        assert_eq!(p("10.0.0.2"), ("10.0.0.2".into(), 3240));
        assert_eq!(p("::1"), ("::1".into(), 3240));
        assert_eq!(p("[::1]:4000"), ("::1".into(), 4000));
        assert!(Server::parse("usbgw:http").is_err());
        assert!(Server::parse(":3240").is_err());
    }

    #[test]
    fn display_roundtrips() {
        for s in ["usbgw.local:3240", "[::1]:4000", "10.0.0.2:3240"] {
            assert_eq!(Server::parse(s).unwrap().to_string(), s);
        }
    }
}
