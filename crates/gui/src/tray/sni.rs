//! StatusNotifierItem tray for Linux (Waybar, KDE, GNOME with the
//! AppIndicator extension). The panel draws the menu; a left click opens the
//! window.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};

use anyhow::{anyhow, Result};
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::MenuItem;

use super::{icon_rgba, toggle, view, Entry, Shared, View, ICON_SIZE, TICK};
use crate::launch;
use crate::model::Action;

struct Tray {
    view: View,
    model: Shared,
    actions: Sender<Action>,
    quit: Sender<()>,
}

pub fn run(model: Shared, actions: Sender<Action>) -> Result<()> {
    let (quit, quit_rx) = mpsc::channel();
    let mut shown = view(&model.lock().unwrap());
    let tray = Tray {
        view: shown.clone(),
        model: model.clone(),
        actions,
        quit,
    };
    // The panel may start after us (both start with the session), so wait for
    // it instead of failing.
    let handle = tray
        .assume_sni_available(true)
        .spawn()
        .map_err(|e| anyhow!("creating the tray icon: {e}"))?;
    loop {
        match quit_rx.recv_timeout(TICK) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                handle.shutdown().wait();
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let v = view(&model.lock().unwrap());
        if v != shown {
            shown = v.clone();
            if handle.update(|t| t.view = v).is_none() {
                return Err(anyhow!("the tray icon service stopped"));
            }
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "farplug".into()
    }

    fn title(&self) -> String {
        "farplug".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let mut argb = icon_rgba(self.view.health);
        for px in argb.as_chunks_mut::<4>().0 {
            px.rotate_right(1);
        }
        vec![ksni::Icon {
            width: ICON_SIZE as i32,
            height: ICON_SIZE as i32,
            data: argb,
        }]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.view.tooltip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        launch::open_window();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        for e in &self.view.entries {
            items.push(match e {
                Entry::Note(text) | Entry::Header(text) => StandardItem {
                    label: escape(text),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                Entry::Separator => MenuItem::Separator,
                Entry::Device {
                    server,
                    id,
                    label,
                    enabled,
                    attached,
                } => {
                    let (server, id) = (server.clone(), id.clone());
                    CheckmarkItem {
                        label: escape(label),
                        enabled: *enabled,
                        checked: *attached,
                        activate: Box::new(move |t: &mut Self| {
                            toggle(&t.model, &t.actions, &server, &id)
                        }),
                        ..Default::default()
                    }
                    .into()
                }
            });
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open farplug…".into(),
                activate: Box::new(|_: &mut Self| launch::open_window()),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit tray icon".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.quit.send(());
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// A single underscore marks the keyboard accelerator in D-Bus menus.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}
