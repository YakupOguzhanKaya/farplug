//! The tray icon: a menu of devices to toggle, and a way to open the window.
//! Its color shows the overall state: green when everything is attached,
//! amber while something is retrying, grey when nothing is attached.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use farplug::ipc::DeviceState;
use farplug::paths::Paths;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::launch;
use crate::model::{self, Action, Model};

const TICK: Duration = Duration::from_millis(100);
const SEP: char = '\u{1f}';

#[derive(Clone, Copy, PartialEq, Eq)]
enum Health {
    Idle,
    Ok,
    Retrying,
}

pub fn run() -> Result<()> {
    // Both XDG autostart and the systemd user unit may start the tray.
    #[cfg(target_os = "linux")]
    let Some(_instance) = single_instance() else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    gtk::init().map_err(|e| anyhow!("starting GTK for the tray icon: {e}"))?;

    let (model, actions) = model::spawn(Paths::default(), || {});
    let mut health = Health::Idle;
    let tray = TrayIconBuilder::new()
        .with_tooltip("farplug")
        .with_icon(icon(health))
        .with_menu(Box::new(Menu::new()))
        .build()
        .map_err(|e| anyhow!("creating the tray icon: {e}"))?;

    let mut shown_menu = String::new();
    loop {
        pump();
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if !on_click(ev.id.0.as_str(), &model.lock().unwrap(), &actions) {
                return Ok(());
            }
        }

        let m = model.lock().unwrap();
        let (signature, h) = summarize(&m);
        if signature != shown_menu {
            tray.set_menu(Some(Box::new(build_menu(&m)?)));
            let _ = tray.set_tooltip(Some(tooltip(&m)));
            shown_menu = signature;
        }
        if h != health {
            let _ = tray.set_icon(Some(icon(h)));
            health = h;
        }
        drop(m);
        thread::sleep(TICK);
    }
}

/// Returns false to quit.
fn on_click(id: &str, m: &Model, actions: &Sender<Action>) -> bool {
    match id {
        "open" => launch::open_window(),
        "quit" => return false,
        _ => {
            if let Some((server, dev)) = id.split_once(SEP) {
                let attached = m
                    .rows(server)
                    .iter()
                    .any(|r| r.id == dev && r.status.is_some());
                let (server, id) = (server.to_owned(), dev.to_owned());
                let _ = actions.send(if attached {
                    Action::Detach { server, id }
                } else {
                    Action::Attach { server, id }
                });
            }
        }
    }
    true
}

/// A string that changes whenever the menu would, plus the overall health.
fn summarize(m: &Model) -> (String, Health) {
    let mut sig = String::new();
    let (mut any, mut retrying) = (false, false);
    if let Some(Err(e)) = &m.agent {
        sig.push_str(e);
    }
    for server in m.servers() {
        sig.push_str(&server);
        for r in m.rows(&server) {
            let state = r.status.map(|s| s.state);
            any |= state.is_some();
            retrying |= matches!(state, Some(DeviceState::Retrying | DeviceState::Connecting));
            sig.push_str(&format!(
                "|{}{:?}{:?}{:?}",
                r.id,
                state,
                r.description(),
                r.used_elsewhere()
            ));
        }
    }
    let health = match (any, retrying) {
        (_, true) => Health::Retrying,
        (true, false) => Health::Ok,
        (false, false) => Health::Idle,
    };
    (sig, health)
}

fn build_menu(m: &Model) -> Result<Menu> {
    let menu = Menu::new();
    let err = |e: tray_icon::menu::Error| anyhow!("building the tray menu: {e}");
    match &m.agent {
        Some(Err(e)) => {
            menu.append(&MenuItem::new(
                format!("Agent unavailable: {e}"),
                false,
                None,
            ))
            .map_err(err)?;
        }
        None => menu
            .append(&MenuItem::new("Connecting to the agent…", false, None))
            .map_err(err)?,
        Some(Ok(_)) if m.servers().is_empty() => menu
            .append(&MenuItem::new(
                "No servers yet; open farplug to add one",
                false,
                None,
            ))
            .map_err(err)?,
        Some(Ok(_)) => {}
    }
    for (i, server) in m.servers().into_iter().enumerate() {
        if i > 0 {
            menu.append(&PredefinedMenuItem::separator()).map_err(err)?;
        }
        menu.append(&MenuItem::new(&server, false, None))
            .map_err(err)?;
        for r in m.rows(&server) {
            let name = match r.description() {
                Some(d) => format!("{} \u{2014} {d}", r.id),
                None => r.id.to_owned(),
            };
            let busy = r.used_elsewhere();
            let label = match (r.status.map(|s| s.state), &busy) {
                (Some(DeviceState::Attached), _) => name,
                (Some(_), _) => format!("{name} (reconnecting)"),
                (None, Some(b)) => format!("{name} ({b})"),
                (None, None) => name,
            };
            menu.append(&CheckMenuItem::with_id(
                format!("{server}{SEP}{}", r.id),
                label,
                busy.is_none(),
                r.status.is_some(),
                None,
            ))
            .map_err(err)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator()).map_err(err)?;
    menu.append(&MenuItem::with_id("open", "Open farplug…", true, None))
        .map_err(err)?;
    menu.append(&MenuItem::with_id("quit", "Quit tray icon", true, None))
        .map_err(err)?;
    Ok(menu)
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

/// A ring with a filled center, drawn rather than shipped as an image.
fn icon(h: Health) -> Icon {
    const N: u32 = 32;
    let color = match h {
        Health::Idle => [0x8a, 0x8f, 0x98],
        Health::Ok => [0x2e, 0xa0, 0x43],
        Health::Retrying => [0xd2, 0x99, 0x22],
    };
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let on = d <= 6.5 || (10.5..=15.0).contains(&d);
            rgba.extend_from_slice(&color);
            rgba.push(if on { 0xff } else { 0 });
        }
    }
    Icon::from_rgba(rgba, N, N).expect("icon dimensions match its buffer")
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

#[cfg(target_os = "linux")]
fn pump() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

#[cfg(windows)]
fn pump() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
