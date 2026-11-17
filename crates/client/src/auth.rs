//! Who may control the Linux agent: root, the agent's own user, and members
//! of the `farplug` group. Membership is read from /etc/group when a client
//! connects, so a user added to the group can use farplug right away instead
//! of after logging in again.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

pub const GROUP: &str = "farplug";

pub struct Peer {
    pub pid: i32,
    pub uid: u32,
}

pub fn peer(sock: &UnixStream) -> io::Result<Peer> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are valid for the duration of the call, and len
    // holds cred's size.
    let rc = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Peer {
        pid: cred.pid,
        uid: cred.uid,
    })
}

pub fn authorize(peer: &Peer) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    if peer.uid == 0 || peer.uid == unsafe { libc::geteuid() } {
        return Ok(());
    }
    let groups = fs::read_to_string("/etc/group").unwrap_or_default();
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let user = passwd_entry(&passwd, peer.uid);
    let name = user
        .as_ref()
        .map_or_else(|| peer.uid.to_string(), |u| u.0.clone());
    let Some((gid, members)) = group_entry(&groups, GROUP) else {
        return Err(format!(
            "permission denied: the `{GROUP}` group does not exist; \
             create it and add yourself: sudo groupadd --system {GROUP} && sudo usermod -aG {GROUP} {name}"
        ));
    };
    let in_session = fs::read_to_string(format!("/proc/{}/status", peer.pid))
        .map(|s| proc_groups(&s).contains(&gid))
        .unwrap_or(false);
    let listed = user
        .as_ref()
        .is_some_and(|(n, primary)| *primary == gid || members.iter().any(|m| m == n));
    if in_session || listed {
        Ok(())
    } else {
        Err(format!(
            "permission denied: {name} is not in the `{GROUP}` group; \
             add yourself with: sudo usermod -aG {GROUP} {name}"
        ))
    }
}

/// (gid, members) of a group in /etc/group.
pub fn group_entry(text: &str, name: &str) -> Option<(u32, Vec<String>)> {
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 4 || f[0] != name {
            return None;
        }
        let members = f[3]
            .split(',')
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .collect();
        Some((f[2].parse().ok()?, members))
    })
}

/// (name, primary gid) of a user in /etc/passwd.
fn passwd_entry(text: &str, uid: u32) -> Option<(String, u32)> {
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 4 || f[2].parse::<u32>().ok()? != uid {
            return None;
        }
        Some((f[0].to_owned(), f[3].parse().ok()?))
    })
}

/// Supplementary groups from /proc/<pid>/status.
fn proc_groups(status: &str) -> Vec<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Groups:"))
        .map(|g| {
            g.split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_system_files() {
        let group = "root:x:0:\nwheel:x:10:alice\nfarplug:x:972:alice,bob\n";
        assert_eq!(
            group_entry(group, "farplug"),
            Some((972, vec!["alice".into(), "bob".into()]))
        );
        assert_eq!(
            group_entry("farplug:x:972:\n", "farplug"),
            Some((972, vec![]))
        );
        assert_eq!(group_entry(group, "nosuch"), None);

        let passwd = "root:x:0:0::/root:/bin/sh\nalice:x:1000:1000::/home/alice:/bin/zsh\n";
        assert_eq!(passwd_entry(passwd, 1000), Some(("alice".into(), 1000)));
        assert_eq!(passwd_entry(passwd, 1001), None);

        let status = "Name:\tzsh\nUid:\t1000\t1000\t1000\t1000\nGroups:\t10 972 998 \n";
        assert_eq!(proc_groups(status), vec![10, 972, 998]);
        assert_eq!(proc_groups("Groups:\t\n"), Vec::<u32>::new());
    }
}
