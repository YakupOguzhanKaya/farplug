//! `.local` name lookup over multicast DNS. The Linux client is a static musl
//! binary, and musl's resolver cannot use the system's mDNS plugin
//! (nss-mdns), so `.local` names would otherwise never resolve.

use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use mdns_sd::{HostnameResolutionEvent, ScopedIp, ServiceDaemon};

pub fn is_mdns_name(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h.ends_with(".local") && h.len() > ".local".len()
}

/// IPv4 addresses first: they are what a homelab server is usually reached by.
pub fn resolve(host: &str, port: u16, timeout: Duration) -> Result<Vec<SocketAddr>> {
    let name = format!("{}.", host.trim_end_matches('.'));
    let daemon = ServiceDaemon::new().map_err(|e| anyhow!("starting mDNS: {e}"))?;
    let result = lookup(&daemon, &name, port, timeout);
    let _ = daemon.shutdown();
    result
}

fn lookup(
    daemon: &ServiceDaemon,
    name: &str,
    port: u16,
    timeout: Duration,
) -> Result<Vec<SocketAddr>> {
    let events = daemon
        .resolve_hostname(name, Some(timeout.as_millis() as u64))
        .map_err(|e| anyhow!("mDNS lookup of {name}: {e}"))?;
    let deadline = Instant::now() + timeout;
    while let Ok(ev) = events.recv_deadline(deadline) {
        match ev {
            HostnameResolutionEvent::AddressesFound(_, ips) => {
                let mut addrs: Vec<SocketAddr> =
                    ips.iter().map(|ip| socket_addr(ip, port)).collect();
                addrs.sort_by_key(|a| a.is_ipv6());
                return Ok(addrs);
            }
            HostnameResolutionEvent::SearchTimeout(_) => break,
            _ => {}
        }
    }
    bail!("no reply to an mDNS query for {name}")
}

fn socket_addr(ip: &ScopedIp, port: u16) -> SocketAddr {
    match (ip, ip.to_ip_addr()) {
        (ScopedIp::V6(v6), IpAddr::V6(a)) if a.is_unicast_link_local() => {
            SocketAddrV6::new(a, port, 0, v6.scope_id().index).into()
        }
        (_, a) => SocketAddr::new(a, port),
    }
}

#[cfg(test)]
mod tests {
    use super::is_mdns_name;

    #[test]
    fn recognizes_local_names() {
        assert!(is_mdns_name("enviropi.local"));
        assert!(is_mdns_name("EnviroPi.LOCAL."));
        assert!(!is_mdns_name("local"));
        assert!(!is_mdns_name(".local"));
        assert!(!is_mdns_name("usbgw.lan"));
        assert!(!is_mdns_name("192.168.2.144"));
    }
}
