//! Who may control the agent (Linux and macOS, where it runs as root): root,
//! the agent's own user, and members of the `farplug` group. Membership is
//! looked up when a client connects, so a user added to the group can use
//! farplug right away instead of after logging in again.

#[cfg(target_os = "linux")]
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

pub const GROUP: &str = "farplug";

pub struct Peer {
    #[cfg(target_os = "linux")]
    pub pid: i32,
    pub uid: u32,
}

#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
pub fn peer(sock: &UnixStream) -> io::Result<Peer> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: both out-pointers are valid for the call.
    if unsafe { libc::getpeereid(sock.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Peer { uid })
}

pub fn authorize(peer: &Peer) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    if peer.uid == 0 || peer.uid == unsafe { libc::geteuid() } {
        return Ok(());
    }
    let name = user_name(peer.uid).unwrap_or_else(|| peer.uid.to_string());
    match is_member(peer) {
        Some(true) => Ok(()),
        Some(false) => Err(format!(
            "permission denied: {name} is not in the `{GROUP}` group; \
             add yourself with: {}",
            add_command(&name)
        )),
        None => Err(format!(
            "permission denied: the `{GROUP}` group does not exist; \
             create it and add yourself: {} && {}",
            create_command(),
            add_command(&name)
        )),
    }
}

/// The group's id, for handing files such as serial ports to its members.
pub fn group_id() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let groups = fs::read_to_string("/etc/group").ok()?;
        group_entry(&groups, GROUP).map(|(gid, _)| gid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let name = std::ffi::CString::new(GROUP).ok()?;
        // SAFETY: getgrnam returns null or a pointer valid until the next call.
        let gr = unsafe { libc::getgrnam(name.as_ptr()) };
        // SAFETY: checked for null.
        (!gr.is_null()).then(|| unsafe { (*gr).gr_gid })
    }
}

#[cfg(target_os = "linux")]
fn is_member(peer: &Peer) -> Option<bool> {
    let groups = fs::read_to_string("/etc/group").unwrap_or_default();
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let (gid, members) = group_entry(&groups, GROUP)?;
    let in_session = fs::read_to_string(format!("/proc/{}/status", peer.pid))
        .map(|s| proc_groups(&s).contains(&gid))
        .unwrap_or(false);
    let listed = passwd_entry(&passwd, peer.uid)
        .is_some_and(|(n, primary)| primary == gid || members.contains(&n));
    Some(in_session || listed)
}

/// Asks Directory Services, so a just-added member is seen at once.
#[cfg(not(target_os = "linux"))]
fn is_member(peer: &Peer) -> Option<bool> {
    let gid = group_id()?;
    let (name, primary) = passwd(peer.uid)?;
    if primary == gid {
        return Some(true);
    }
    let cname = std::ffi::CString::new(name).ok()?;
    let mut groups = vec![0 as libc::c_int; 256];
    let mut n = groups.len() as libc::c_int;
    // SAFETY: groups holds n entries; getgrouplist writes at most n.
    unsafe {
        libc::getgrouplist(
            cname.as_ptr(),
            primary as libc::c_int,
            groups.as_mut_ptr(),
            &mut n,
        )
    };
    Some(groups[..n.max(0) as usize].iter().any(|&g| g as u32 == gid))
}

#[cfg(target_os = "linux")]
fn user_name(uid: u32) -> Option<String> {
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    passwd_entry(&passwd, uid).map(|(n, _)| n)
}

#[cfg(not(target_os = "linux"))]
fn user_name(uid: u32) -> Option<String> {
    passwd(uid).map(|(n, _)| n)
}

/// (name, primary gid) from the user database.
#[cfg(not(target_os = "linux"))]
fn passwd(uid: u32) -> Option<(String, u32)> {
    let mut pw = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut out = std::ptr::null_mut();
    // SAFETY: pw, buf and out are valid; getpwuid_r fills pw from buf.
    let rc =
        unsafe { libc::getpwuid_r(uid, pw.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so pw is initialized and pw_name points into buf.
    let pw = unsafe { pw.assume_init() };
    let name = unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }
        .to_string_lossy()
        .into_owned();
    Some((name, pw.pw_gid))
}

#[cfg(target_os = "linux")]
fn add_command(name: &str) -> String {
    format!("sudo usermod -aG {GROUP} {name}")
}

#[cfg(not(target_os = "linux"))]
fn add_command(name: &str) -> String {
    format!("sudo dseditgroup -o edit -a {name} -t user {GROUP}")
}

#[cfg(target_os = "linux")]
fn create_command() -> String {
    format!("sudo groupadd --system {GROUP}")
}

#[cfg(not(target_os = "linux"))]
fn create_command() -> String {
    format!("sudo dseditgroup -o create {GROUP}")
}

/// (gid, members) of a group in /etc/group.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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

#[cfg(all(test, target_os = "linux"))]
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
