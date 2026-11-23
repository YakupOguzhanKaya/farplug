//! Windows tray via `tray-icon`: double-click opens the window, right-click
//! shows the menu.

use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{anyhow, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, TrayIconBuilder, TrayIconEvent};

use super::{icon_rgba, toggle, view, Entry, Health, Shared, View, ICON_SIZE, TICK};
use crate::launch;
use crate::model::Action;

const SEP: char = '\u{1f}';

pub fn run(model: Shared, actions: Sender<Action>) -> Result<()> {
    let tray = TrayIconBuilder::new()
        .with_tooltip("farplug")
        .with_icon(icon(Health::Idle))
        .with_menu(Box::new(Menu::new()))
        .build()
        .map_err(|e| anyhow!("creating the tray icon: {e}"))?;

    let mut shown: Option<View> = None;
    loop {
        pump();
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = ev
            {
                launch::open_window();
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            match ev.id.0.as_str() {
                "open" => launch::open_window(),
                "quit" => return Ok(()),
                other => {
                    if let Some((server, id)) = other.split_once(SEP) {
                        toggle(&model, &actions, server, id);
                    }
                }
            }
        }

        let v = view(&model.lock().unwrap());
        if shown.as_ref() != Some(&v) {
            if shown.as_ref().map(|s| s.health) != Some(v.health) {
                let _ = tray.set_icon(Some(icon(v.health)));
            }
            tray.set_menu(Some(Box::new(build_menu(&v)?)));
            let _ = tray.set_tooltip(Some(&v.tooltip));
            shown = Some(v);
        }
        thread::sleep(TICK);
    }
}

fn build_menu(v: &View) -> Result<Menu> {
    let menu = Menu::new();
    let err = |e: tray_icon::menu::Error| anyhow!("building the tray menu: {e}");
    for e in &v.entries {
        match e {
            Entry::Note(text) | Entry::Header(text) => menu
                .append(&MenuItem::new(text, false, None))
                .map_err(err)?,
            Entry::Separator => menu.append(&PredefinedMenuItem::separator()).map_err(err)?,
            Entry::Device {
                server,
                id,
                label,
                enabled,
                attached,
            } => menu
                .append(&CheckMenuItem::with_id(
                    format!("{server}{SEP}{id}"),
                    label,
                    *enabled,
                    *attached,
                    None,
                ))
                .map_err(err)?,
        }
    }
    menu.append(&PredefinedMenuItem::separator()).map_err(err)?;
    menu.append(&MenuItem::with_id("open", "Open farplug…", true, None))
        .map_err(err)?;
    menu.append(&MenuItem::with_id("quit", "Quit tray icon", true, None))
        .map_err(err)?;
    Ok(menu)
}

fn icon(h: Health) -> Icon {
    Icon::from_rgba(icon_rgba(h), ICON_SIZE, ICON_SIZE).expect("icon dimensions match its buffer")
}

fn pump() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    // SAFETY: a plain Win32 message loop over this thread's queue.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
