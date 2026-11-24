//! Windows tray via `tray-icon`: double-click opens the window, right-click
//! shows the menu.

use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{anyhow, Result};
use tray_icon::menu::Menu;
use tray_icon::{MouseButton, TrayIconBuilder, TrayIconEvent};

use super::menu::{build_menu, handle_clicks, icon};
use super::{view, Health, Shared, View, TICK};
use crate::launch;
use crate::model::Action;

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
        if !handle_clicks(&model, &actions) {
            return Ok(());
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
