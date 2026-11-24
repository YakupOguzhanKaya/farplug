//! The menu and icon for `tray-icon`, which Windows and macOS use.

use std::sync::mpsc::Sender;

use anyhow::{anyhow, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::Icon;

use super::{icon_rgba, toggle, Entry, Health, Shared, View, ICON_SIZE};
use crate::launch;
use crate::model::Action;

const SEP: char = '\u{1f}';

/// Handles the menu clicks so far; false when Quit was chosen.
pub fn handle_clicks(model: &Shared, actions: &Sender<Action>) -> bool {
    while let Ok(ev) = MenuEvent::receiver().try_recv() {
        match ev.id.0.as_str() {
            "open" => launch::open_window(),
            "quit" => return false,
            other => {
                if let Some((server, id)) = other.split_once(SEP) {
                    toggle(model, actions, server, id);
                }
            }
        }
    }
    true
}

pub fn build_menu(v: &View) -> Result<Menu> {
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

pub fn icon(h: Health) -> Icon {
    Icon::from_rgba(icon_rgba(h), ICON_SIZE, ICON_SIZE).expect("icon dimensions match its buffer")
}
