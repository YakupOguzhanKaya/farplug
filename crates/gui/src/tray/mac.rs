//! macOS menu-bar icon via `tray-icon`. Status items need a running
//! NSApplication, so this runs a window-less winit event loop, as an
//! accessory app: no Dock icon and no app menu of its own.
//!
//! It also types and moves the pointer for remote keyboards and mice: the
//! root agent streams their events, and only a program in the user's session
//! can post them.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tray_icon::menu::Menu;
use tray_icon::{TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
use winit::window::WindowId;

use farplug::macinput::{self, MacInput};
use farplug::paths::Paths;
use farplug::{hid::Input, ipc};

use super::menu::{build_menu, handle_clicks, icon};
use super::{view, Health, Shared, View, TICK};
use crate::model::Action;

struct App {
    model: Shared,
    actions: Sender<Action>,
    tray: Option<TrayIcon>,
    shown: Option<View>,
    error: Option<anyhow::Error>,
}

pub fn run(model: Shared, actions: Sender<Action>) -> Result<()> {
    thread::spawn(relay_input);
    let event_loop = EventLoop::builder()
        .with_activation_policy(ActivationPolicy::Accessory)
        .build()
        .map_err(|e| anyhow!("starting the menu-bar icon: {e}"))?;
    let mut app = App {
        model,
        actions,
        tray: None,
        shown: None,
        error: None,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow!("running the menu-bar icon: {e}"))?;
    app.error.map_or(Ok(()), Err)
}

impl ApplicationHandler for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        // The status item can only be made once the application has launched.
        if cause == StartCause::Init {
            let tray = TrayIconBuilder::new()
                .with_tooltip("farplug")
                .with_icon(icon(Health::Idle))
                .with_icon_as_template(false)
                .with_menu(Box::new(Menu::new()))
                .build();
            match tray {
                Ok(t) => self.tray = Some(t),
                Err(e) => {
                    self.error = Some(anyhow!("creating the menu-bar icon: {e}"));
                    event_loop.exit();
                }
            }
        }
    }

    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !handle_clicks(&self.model, &self.actions) {
            event_loop.exit();
            return;
        }
        if let Some(tray) = &self.tray {
            let v = view(&self.model.lock().unwrap());
            if self.shown.as_ref() != Some(&v) {
                if self.shown.as_ref().map(|s| s.health) != Some(v.health) {
                    let _ = tray.set_icon(Some(icon(v.health)));
                }
                match build_menu(&v) {
                    Ok(menu) => tray.set_menu(Some(Box::new(menu))),
                    Err(e) => tracing::warn!(error = %e, "building the menu"),
                }
                let _ = tray.set_tooltip(Some(&v.tooltip));
                self.shown = Some(v);
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + TICK));
    }
}

/// Posts the agent's input events while this app has the Accessibility
/// permission, asking for it once.
fn relay_input() {
    let paths = Paths::default();
    let mut asked = false;
    loop {
        if !macinput::trusted(!asked) {
            asked = true;
            thread::sleep(Duration::from_secs(3));
            continue;
        }
        match (MacInput::new(), ipc::input_events(&paths)) {
            (Ok(mut input), Ok(events)) => {
                for e in events {
                    match e {
                        Ok(e) => input.send(e),
                        Err(e) => {
                            tracing::debug!(error = %format!("{e:#}"), "input stream ended");
                            break;
                        }
                    }
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::debug!(error = %format!("{e:#}"), "not relaying input");
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}
