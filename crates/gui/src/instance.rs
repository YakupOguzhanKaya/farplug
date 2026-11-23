//! One window at a time: a second launch asks the open window to come
//! forward, then exits. The window listens on a loopback port it records in a
//! per-user file.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const SHOW: &str = "show";
/// The reply that proves the port belongs to a farplug window and not to
/// whatever reused it after one crashed.
const ACK: &str = "farplug";

pub struct Instance {
    listener: TcpListener,
    file: PathBuf,
}

pub enum Claim {
    /// Another window was found and asked to come forward.
    Elsewhere,
    /// This launch opens the window; None if it could not listen for others.
    Here(Option<Instance>),
}

pub fn claim() -> Claim {
    let file = runtime_dir().join("window.port");
    if let Some(port) = fs::read_to_string(&file)
        .ok()
        .and_then(|p| p.trim().parse().ok())
    {
        if ask_to_show(port) {
            return Claim::Elsewhere;
        }
    }
    let listen = || -> Option<Instance> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        if let Some(dir) = file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        fs::write(&file, port.to_string()).ok()?;
        Some(Instance {
            listener,
            file: file.clone(),
        })
    };
    Claim::Here(listen())
}

impl Instance {
    /// Calls `show` whenever another launch asks for this window.
    pub fn serve(&self, show: impl Fn() + Send + 'static) {
        let Ok(listener) = self.listener.try_clone() else {
            return;
        };
        thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
                let mut line = String::new();
                if BufReader::new(&conn).read_line(&mut line).is_ok() && line.trim() == SHOW {
                    let _ = writeln!(conn, "{ACK}");
                    show();
                }
            }
        });
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.file);
    }
}

fn ask_to_show(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
    if writeln!(s, "{SHOW}").is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply).is_ok() && reply.trim() == ACK
}

#[cfg(target_os = "linux")]
fn runtime_dir() -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("farplug"),
        None => {
            let uid = fs::metadata("/proc/self").map_or(0, |m| m.uid());
            std::env::temp_dir().join(format!("farplug-{uid}"))
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn runtime_dir() -> PathBuf {
    farplug::paths::Paths::default()
        .endpoint
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}
