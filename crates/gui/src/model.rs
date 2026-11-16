//! What the GUI shows, kept fresh by background threads so the UI thread
//! never blocks on the network or the agent.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use farplug::ipc::{self, DeviceStatus, Request, Status};
use farplug::paths::Paths;
use farplug::Server;
use farplug_usbip::ExportedDevice;

const AGENT_POLL: Duration = Duration::from_secs(2);
const SERVER_POLL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Model {
    /// The agent's view, or why it could not be reached.
    pub agent: Option<Result<Status, String>>,
    /// Per server: what it exports, or why it could not be listed.
    pub exported: BTreeMap<String, Result<Vec<ExportedDevice>, String>>,
    pub last_error: Option<String>,
}

pub enum Action {
    Attach { server: String, id: String },
    Detach { server: String, id: String },
    AddServer(String),
    RemoveServer(String),
}

/// One line of a server's device table.
pub struct Row<'a> {
    pub id: &'a str,
    pub exported: Option<&'a ExportedDevice>,
    pub status: Option<&'a DeviceStatus>,
}

impl Model {
    pub fn servers(&self) -> Vec<String> {
        match &self.agent {
            Some(Ok(s)) => s.servers.clone(),
            _ => Vec::new(),
        }
    }

    /// Devices the server exports, plus ones the agent is trying to attach
    /// that the server does not list right now (unplugged, or the server is
    /// down).
    pub fn rows(&self, server: &str) -> Vec<Row<'_>> {
        let statuses: Vec<&DeviceStatus> = match &self.agent {
            Some(Ok(s)) => s.devices.iter().filter(|d| d.server == server).collect(),
            _ => Vec::new(),
        };
        let mut rows: Vec<Row> = match self.exported.get(server) {
            Some(Ok(devs)) => devs
                .iter()
                .map(|d| Row {
                    id: &d.device.busid,
                    exported: Some(d),
                    status: statuses.iter().copied().find(|s| s.id == d.device.busid),
                })
                .collect(),
            _ => Vec::new(),
        };
        for s in statuses {
            if !rows.iter().any(|r| r.id == s.id) {
                rows.push(Row {
                    id: &s.id,
                    exported: None,
                    status: Some(s),
                });
            }
        }
        rows.sort_by(|a, b| a.id.cmp(b.id));
        rows
    }
}

/// Starts the background threads; `repaint` is called whenever the model
/// changes.
pub fn spawn(
    paths: Paths,
    repaint: impl Fn() + Send + Sync + 'static,
) -> (Arc<Mutex<Model>>, Sender<Action>) {
    let model = Arc::new(Mutex::new(Model::default()));
    let repaint = Arc::new(repaint);
    let (tx, rx) = mpsc::channel::<Action>();

    let (m, r) = (model.clone(), repaint.clone());
    thread::spawn(move || loop {
        crate::launch::ensure_agent(&paths);
        let action = match rx.recv_timeout(AGENT_POLL) {
            Ok(a) => Some(a),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let req = match action {
            None => Request::Status,
            Some(Action::Attach { server, id }) => Request::Attach {
                server,
                devices: vec![id],
            },
            Some(Action::Detach { server, id }) => Request::Detach {
                server: Some(server),
                devices: vec![id],
            },
            Some(Action::AddServer(server)) => Request::AddServer { server },
            Some(Action::RemoveServer(server)) => Request::RemoveServer { server },
        };
        let is_action = req != Request::Status;
        let result = ipc::call(&paths, req);
        {
            let mut m = m.lock().unwrap();
            match result {
                Ok(s) => {
                    m.agent = Some(Ok(s));
                    if is_action {
                        m.last_error = None;
                    }
                }
                Err(e) if is_action => m.last_error = Some(format!("{e:#}")),
                Err(e) => m.agent = Some(Err(format!("{e:#}"))),
            }
        }
        r();
    });

    let (m, r) = (model.clone(), repaint);
    thread::spawn(move || loop {
        let servers = m.lock().unwrap().servers();
        let fetches: Vec<_> = servers
            .into_iter()
            .map(|s| {
                thread::spawn(move || {
                    let result = Server::parse(&s)
                        .and_then(|server| server.devices())
                        .map_err(|e| format!("{e:#}"));
                    (s, result)
                })
            })
            .collect();
        let results: BTreeMap<_, _> = fetches.into_iter().filter_map(|h| h.join().ok()).collect();
        m.lock().unwrap().exported = results;
        r();
        thread::sleep(SERVER_POLL);
    });

    (model, tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use farplug::ipc::DeviceState;
    use farplug_usbip::{Speed, UsbDevice};

    fn exported(id: &str) -> ExportedDevice {
        ExportedDevice {
            device: UsbDevice {
                path: String::new(),
                busid: id.into(),
                busnum: 1,
                devnum: 2,
                speed: Speed::Full,
                vendor_id: 0x10c4,
                product_id: 0xea60,
                bcd_device: 0,
                class: 0,
                subclass: 0,
                protocol: 0,
                configuration_value: 1,
                num_configurations: 1,
                num_interfaces: 0,
            },
            interfaces: vec![],
        }
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn rows_merge_exported_and_attached() {
        let mut m = Model::default();
        m.agent = Some(Ok(Status {
            servers: vec!["gw:3240".into()],
            devices: vec![
                DeviceStatus {
                    server: "gw:3240".into(),
                    id: "zigbee".into(),
                    state: DeviceState::Attached,
                    port: Some(1),
                    error: None,
                },
                DeviceStatus {
                    server: "gw:3240".into(),
                    id: "gone".into(),
                    state: DeviceState::Retrying,
                    port: None,
                    error: Some("no such device".into()),
                },
            ],
        }));
        m.exported.insert(
            "gw:3240".into(),
            Ok(vec![exported("zwave"), exported("zigbee")]),
        );

        let rows = m.rows("gw:3240");
        let ids: Vec<&str> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, ["gone", "zigbee", "zwave"]);
        assert!(rows[0].exported.is_none() && rows[0].status.is_some());
        assert!(rows[1].exported.is_some() && rows[1].status.is_some());
        assert!(rows[2].exported.is_some() && rows[2].status.is_none());
    }
}
