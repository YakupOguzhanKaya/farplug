//! The background agent: owns every attachment, remembers which devices to
//! keep attached, and takes orders from the CLI and GUI over [`crate::ipc`].

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::ipc::{self, DeviceState, DeviceStatus, Request, Status};
use crate::paths::Paths;
use crate::supervisor::keep_attached;
use crate::{Backend, Server};

/// (normalized server, device id)
type Key = (String, String);

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    servers: Vec<String>,
    devices: Vec<SavedDevice>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedDevice {
    server: String,
    id: String,
}

struct Worker {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<DeviceStatus>>,
    handle: JoinHandle<()>,
}

pub struct Agent {
    backend: Arc<dyn Backend>,
    state_path: std::path::PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    servers: Vec<String>,
    workers: BTreeMap<Key, Worker>,
}

/// Run the agent until SIGINT/SIGTERM (or Ctrl-C), then detach everything.
pub fn run(paths: Paths) -> Result<()> {
    let agent = Arc::new(Agent::new(crate::backend::platform()?, &paths)?);
    listen(agent.clone(), &paths)?;

    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })
    .context("installing the signal handler")?;
    let _ = rx.recv();
    info!("stopping; detaching all devices");
    let _ = fs::remove_file(&paths.endpoint);
    agent.shutdown();
    Ok(())
}

#[cfg(target_os = "linux")]
fn listen(agent: Arc<Agent>, paths: &Paths) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    let path = &paths.endpoint;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if UnixStream::connect(path).is_ok() {
        bail!(
            "another farplug agent is already listening on {}",
            path.display()
        );
    }
    let _ = fs::remove_file(path);
    let listener =
        UnixListener::bind(path).with_context(|| format!("listening on {}", path.display()))?;
    // Anyone may connect; crate::auth decides whose requests are carried out.
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    info!(socket = %path.display(), "agent listening");

    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut sock) = conn else { continue };
            let a = agent.clone();
            thread::spawn(move || {
                let allowed = crate::auth::peer(&sock)
                    .map_err(|e| format!("checking who connected: {e}"))
                    .and_then(|p| crate::auth::authorize(&p));
                serve(&a, &mut sock, "", allowed);
            });
        }
    });
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn listen(agent: Arc<Agent>, paths: &Paths) -> Result<()> {
    use std::net::{Ipv4Addr, TcpListener};

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let token = new_token()?;
    let endpoint = ipc::Endpoint {
        port: listener.local_addr()?.port(),
        token: token.clone(),
    };
    write_atomic(&paths.endpoint, &serde_json::to_vec(&endpoint)?, true)?;
    info!(port = endpoint.port, "agent listening");

    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut sock) = conn else { continue };
            let (a, token) = (agent.clone(), token.clone());
            thread::spawn(move || serve(&a, &mut sock, &token, Ok(())));
        }
    });
    Ok(())
}

fn serve(agent: &Agent, sock: &mut (impl Read + Write), token: &str, allowed: Result<(), String>) {
    let result = ipc::read_request(sock, token).map(|req| {
        debug!(?req, "request");
        match allowed {
            Ok(()) => agent.handle(req),
            Err(e) => Err(anyhow!(e)),
        }
    });
    match result {
        Ok(reply) => {
            let _ = ipc::write_reply(sock, reply);
        }
        Err(e) => debug!(error = %e, "bad request"),
    }
}

impl Agent {
    pub fn new(backend: Arc<dyn Backend>, paths: &Paths) -> Result<Self> {
        let saved: Saved = match fs::read_to_string(&paths.state) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", paths.state.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Saved::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", paths.state.display())),
        };
        let agent = Self {
            backend,
            state_path: paths.state.clone(),
            inner: Mutex::new(Inner {
                servers: saved.servers,
                workers: BTreeMap::new(),
            }),
        };
        {
            let mut inner = agent.inner.lock().unwrap();
            for d in saved.devices {
                match Server::parse(&d.server) {
                    Ok(server) => agent.start(&mut inner, server, d.id),
                    Err(e) => warn!(server = %d.server, error = %e, "skipping saved device"),
                }
            }
        }
        Ok(agent)
    }

    pub fn handle(&self, req: Request) -> Result<Status> {
        let mut inner = self.inner.lock().unwrap();
        match req {
            Request::Status => {}
            Request::AddServer { server } => {
                let s = Server::parse(&server)?.to_string();
                if !inner.servers.contains(&s) {
                    inner.servers.push(s);
                }
                self.save(&inner)?;
            }
            Request::RemoveServer { server } => {
                let s = Server::parse(&server)?.to_string();
                inner.servers.retain(|x| *x != s);
                let keys: Vec<Key> = inner.workers.keys().filter(|k| k.0 == s).cloned().collect();
                for k in keys {
                    self.stop(&mut inner, &k);
                }
                self.save(&inner)?;
            }
            Request::Attach { server, devices } => {
                let server = Server::parse(&server)?;
                let s = server.to_string();
                if !inner.servers.contains(&s) {
                    inner.servers.push(s.clone());
                }
                for id in devices {
                    if id.is_empty() || id.len() > farplug_usbip::MAX_BUSID {
                        bail!(
                            "device id {id:?} must be 1 to {} characters",
                            farplug_usbip::MAX_BUSID
                        );
                    }
                    if !inner.workers.contains_key(&(s.clone(), id.clone())) {
                        self.start(&mut inner, server.clone(), id);
                    }
                }
                self.save(&inner)?;
            }
            Request::Detach { server, devices } => {
                let server = server
                    .map(|s| Server::parse(&s).map(|s| s.to_string()))
                    .transpose()?;
                for id in &devices {
                    let keys: Vec<Key> = inner
                        .workers
                        .keys()
                        .filter(|(s, i)| i == id && server.as_ref().is_none_or(|want| want == s))
                        .cloned()
                        .collect();
                    if keys.is_empty() {
                        bail!("{id} is not attached");
                    }
                    for k in keys {
                        self.stop(&mut inner, &k);
                    }
                }
                self.save(&inner)?;
            }
        }
        Ok(status(&inner))
    }

    fn start(&self, inner: &mut Inner, server: Server, id: String) {
        let key = (server.to_string(), id.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(DeviceStatus {
            server: key.0.clone(),
            id: id.clone(),
            state: DeviceState::Connecting,
            port: None,
            error: None,
        }));
        let (backend, stop2, status2) = (self.backend.clone(), stop.clone(), status.clone());
        let handle = thread::spawn(move || {
            let report = |state, port, error| {
                let mut s = status2.lock().unwrap();
                s.state = state;
                s.port = port;
                s.error = error;
            };
            keep_attached(&server, &id, &*backend, &stop2, &report);
        });
        inner.workers.insert(
            key,
            Worker {
                stop,
                status,
                handle,
            },
        );
    }

    /// Detaching can block for a while, so it finishes in the background.
    fn stop(&self, inner: &mut Inner, key: &Key) {
        if let Some(w) = inner.workers.remove(key) {
            w.stop.store(true, Ordering::Relaxed);
            thread::spawn(move || {
                let _ = w.handle.join();
            });
        }
    }

    fn save(&self, inner: &Inner) -> Result<()> {
        let saved = Saved {
            servers: inner.servers.clone(),
            devices: inner
                .workers
                .keys()
                .map(|(server, id)| SavedDevice {
                    server: server.clone(),
                    id: id.clone(),
                })
                .collect(),
        };
        write_atomic(&self.state_path, &serde_json::to_vec_pretty(&saved)?, false)
    }

    pub fn shutdown(&self) {
        let workers: Vec<Worker> = {
            let mut inner = self.inner.lock().unwrap();
            std::mem::take(&mut inner.workers).into_values().collect()
        };
        for w in &workers {
            w.stop.store(true, Ordering::Relaxed);
        }
        for w in workers {
            let _ = w.handle.join();
        }
    }
}

fn status(inner: &Inner) -> Status {
    Status {
        servers: inner.servers.clone(),
        devices: inner
            .workers
            .values()
            .map(|w| w.status.lock().unwrap().clone())
            .collect(),
    }
}

#[cfg(not(target_os = "linux"))]
fn new_token() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| anyhow!("no randomness: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// `secret` files are readable only by their owner.
fn write_atomic(path: &Path, data: &[u8], secret: bool) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(if secret { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = secret;
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(data)?;
    drop(f);
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Attached;
    use std::time::{Duration, Instant};

    /// Attaches anything except ids starting with "bad".
    struct FakeBackend {
        attached: Mutex<Vec<u32>>,
    }

    impl Backend for FakeBackend {
        fn attach(&self, _: &Server, id: &str) -> Result<Attached> {
            if id.starts_with("bad") {
                bail!("server refused: no such device");
            }
            let mut a = self.attached.lock().unwrap();
            let port = a.len() as u32 + 1;
            a.push(port);
            Ok(Attached { port, devid: 0 })
        }
        fn is_attached(&self, a: &Attached, _: &str) -> bool {
            self.attached.lock().unwrap().contains(&a.port)
        }
        fn detach(&self, a: &Attached) -> Result<()> {
            self.attached.lock().unwrap().retain(|p| *p != a.port);
            Ok(())
        }
    }

    fn setup() -> (tempfile::TempDir, Paths, Arc<FakeBackend>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            endpoint: dir.path().join("agent.json"),
            state: dir.path().join("state.json"),
        };
        let backend = Arc::new(FakeBackend {
            attached: Mutex::new(Vec::new()),
        });
        (dir, paths, backend)
    }

    fn wait_until(agent: &Agent, f: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = agent.handle(Request::Status).unwrap();
            if f(&s) || Instant::now() > deadline {
                return s;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn attach_remember_detach() {
        let (_dir, paths, backend) = setup();
        let agent = Agent::new(backend.clone(), &paths).unwrap();
        agent
            .handle(Request::Attach {
                server: "gw".into(),
                devices: vec!["zigbee".into(), "bad1".into()],
            })
            .unwrap();
        let s = wait_until(&agent, |s| {
            s.devices.iter().all(|d| d.state != DeviceState::Connecting)
        });
        assert_eq!(s.servers, vec!["gw:3240"]);
        let zigbee = s.devices.iter().find(|d| d.id == "zigbee").unwrap();
        assert_eq!(
            (zigbee.state, zigbee.port),
            (DeviceState::Attached, Some(1))
        );
        let bad = s.devices.iter().find(|d| d.id == "bad1").unwrap();
        assert_eq!(bad.state, DeviceState::Retrying);
        assert!(bad.error.as_deref().unwrap().contains("no such device"));

        let saved = fs::read_to_string(&paths.state).unwrap();
        assert!(saved.contains("zigbee") && saved.contains("bad1"));

        let s = agent
            .handle(Request::Detach {
                server: None,
                devices: vec!["zigbee".into()],
            })
            .unwrap();
        assert_eq!(s.devices.len(), 1);
        assert!(!fs::read_to_string(&paths.state).unwrap().contains("zigbee"));
        assert!(agent
            .handle(Request::Detach {
                server: None,
                devices: vec!["zigbee".into()],
            })
            .is_err());
        agent.shutdown();
    }

    #[test]
    fn saved_devices_reattach_on_start() {
        let (_dir, paths, backend) = setup();
        let agent = Agent::new(backend.clone(), &paths).unwrap();
        agent
            .handle(Request::Attach {
                server: "gw:4000".into(),
                devices: vec!["zigbee".into()],
            })
            .unwrap();
        wait_until(&agent, |s| s.devices[0].state == DeviceState::Attached);
        agent.shutdown();
        assert!(
            backend.attached.lock().unwrap().is_empty(),
            "shutdown detaches"
        );

        let agent = Agent::new(backend.clone(), &paths).unwrap();
        let s = wait_until(&agent, |s| s.devices[0].state == DeviceState::Attached);
        assert_eq!(s.devices[0].server, "gw:4000");
        agent.shutdown();
    }

    #[test]
    fn remove_server_detaches_its_devices() {
        let (_dir, paths, backend) = setup();
        let agent = Agent::new(backend, &paths).unwrap();
        agent
            .handle(Request::AddServer {
                server: "other".into(),
            })
            .unwrap();
        agent
            .handle(Request::Attach {
                server: "gw".into(),
                devices: vec!["zigbee".into()],
            })
            .unwrap();
        let s = agent
            .handle(Request::RemoveServer {
                server: "gw:3240".into(),
            })
            .unwrap();
        assert_eq!(s.servers, vec!["other:3240"]);
        assert!(s.devices.is_empty());
        agent.shutdown();
    }
}
