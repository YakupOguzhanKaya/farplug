//! Input events from remote keyboards and mice, handed to whoever replays
//! them locally: on macOS the menu-bar app, which runs in the user's session
//! that the root agent cannot post events into.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;

use crate::hid::{Event, Input};

static SUBSCRIBERS: Mutex<Vec<Sender<Event>>> = Mutex::new(Vec::new());

pub fn subscribe() -> Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    SUBSCRIBERS.lock().unwrap().push(tx);
    rx
}

/// Sends every event to all current subscribers.
pub struct Broadcast;

impl Input for Broadcast {
    fn send(&mut self, e: Event) {
        tracing::debug!(?e, "input");
        SUBSCRIBERS.lock().unwrap().retain(|s| s.send(e).is_ok());
    }
}
