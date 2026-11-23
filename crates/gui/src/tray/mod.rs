//! The tray icon: a menu of devices to toggle, and a way to open the window.
//! Its color shows the overall state: green when everything is attached,
//! amber while something is retrying, grey when nothing is attached.
//!
//! Linux speaks StatusNotifierItem over D-Bus directly (`sni`), which unlike
//! libappindicator reports left clicks; Windows uses `tray-icon` (`win`).

#[cfg(target_os = "linux")]
mod sni;
#[cfg(windows)]
mod win;

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use farplug::ipc::DeviceState;
use farplug::paths::Paths;

use crate::model::{self, Action, Model};

const TICK: Duration = Duration::from_millis(100);

pub fn run() -> Result<()> {
    // Both XDG autostart and the systemd user unit may start the tray.
    #[cfg(target_os = "linux")]
    let Some(_instance) = single_instance() else {
        return Ok(());
    };
    let (model, actions) = model::spawn(Paths::default(), || {});
    #[cfg(target_os = "linux")]
    return sni::run(model, actions);
    #[cfg(windows)]
    return win::run(model, actions);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Health {
    Idle,
    Ok,
    Retrying,
}

/// Everything the tray shows; rebuilt every tick and pushed out only when it
/// changes.
#[derive(Clone, PartialEq, Debug)]
struct View {
    entries: Vec<Entry>,
    tooltip: String,
    health: Health,
}

#[derive(Clone, PartialEq, Debug)]
enum Entry {
    Note(String),
    Header(String),
    Device {
        server: String,
        id: String,
        label: String,
        enabled: bool,
        attached: bool,
    },
    Separator,
}

fn view(m: &Model) -> View {
    let mut entries = Vec::new();
    match &m.agent {
        Some(Err(e)) => entries.push(Entry::Note(format!("Agent unavailable: {e}"))),
        None => entries.push(Entry::Note("Connecting to the agent…".into())),
        Some(Ok(_)) if m.servers().is_empty() => entries.push(Entry::Note(
            "No servers yet; open farplug to add one".into(),
        )),
        Some(Ok(_)) => {}
    }
    let (mut any, mut retrying) = (false, false);
    for (i, server) in m.servers().into_iter().enumerate() {
        if i > 0 {
            entries.push(Entry::Separator);
        }
        entries.push(Entry::Header(match m.alias(&server) {
            Some(a) => format!("{a} ({server})"),
            None => server.clone(),
        }));
        for r in m.rows(&server) {
            let state = r.status.map(|s| s.state);
            any |= state.is_some();
            retrying |= matches!(state, Some(DeviceState::Retrying | DeviceState::Connecting));
            let name = r.label().unwrap_or(r.id).to_owned();
            let busy = r.used_elsewhere();
            let label = match (state, &busy) {
                (Some(DeviceState::Attached), _) => name,
                (Some(_), _) => format!("{name} (reconnecting)"),
                (None, Some(b)) => format!("{name} ({b})"),
                (None, None) => name,
            };
            entries.push(Entry::Device {
                server: server.clone(),
                id: r.id.to_owned(),
                label,
                enabled: busy.is_none(),
                attached: state.is_some(),
            });
        }
    }
    let health = match (any, retrying) {
        (_, true) => Health::Retrying,
        (true, false) => Health::Ok,
        (false, false) => Health::Idle,
    };
    View {
        entries,
        tooltip: tooltip(m),
        health,
    }
}

/// Attach a device that is not attached, detach one that is.
fn toggle(model: &Mutex<Model>, actions: &Sender<Action>, server: &str, id: &str) {
    let attached = model
        .lock()
        .unwrap()
        .rows(server)
        .iter()
        .any(|r| r.id == id && r.status.is_some());
    let (server, id) = (server.to_owned(), id.to_owned());
    let _ = actions.send(if attached {
        Action::Detach { server, id }
    } else {
        Action::Attach { server, id }
    });
}

fn tooltip(m: &Model) -> String {
    let Some(Ok(s)) = &m.agent else {
        return "farplug: agent unavailable".into();
    };
    let attached = s
        .devices
        .iter()
        .filter(|d| d.state == DeviceState::Attached)
        .count();
    let waiting = s.devices.len() - attached;
    match waiting {
        0 => format!("farplug: {attached} attached"),
        n => format!("farplug: {attached} attached, {n} reconnecting"),
    }
}

const ICON_SIZE: u32 = 32;
/// The app's glyph as RGBA; only its alpha is used. See dist/icon/render.sh.
const GLYPH: &[u8; (ICON_SIZE * ICON_SIZE * 4) as usize] =
    include_bytes!("../../assets/glyph-32.rgba");

/// RGBA pixels: the glyph in the color of `h`, so the icon shows the state.
fn icon_rgba(h: Health) -> Vec<u8> {
    let [r, g, b] = match h {
        Health::Idle => [0x8a, 0x8f, 0x98],
        Health::Ok => [0x2e, 0xa0, 0x43],
        Health::Retrying => [0xd2, 0x99, 0x22],
    };
    GLYPH
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| [r, g, b, px[3]])
        .collect()
}

/// Held for as long as this tray runs; None if another one already is.
#[cfg(target_os = "linux")]
fn single_instance() -> Option<std::os::unix::net::UnixListener> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    let uid = std::fs::metadata("/proc/self").map_or(0, |m| m.uid());
    let addr = SocketAddr::from_abstract_name(format!("farplug-tray-{uid}")).ok()?;
    UnixListener::bind_addr(&addr).ok()
}

type Shared = Arc<Mutex<Model>>;
