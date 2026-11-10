//! Keeping one device attached through disconnects.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tracing::{info, warn};

use crate::ipc::DeviceState;
use crate::{Attached, Backend, Server};

const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Progress reports: the state, the local port once attached, and the last
/// error while retrying.
pub type Report<'a> = &'a (dyn Fn(DeviceState, Option<u32>, Option<String>) + Sync);

/// Attach `id` and re-attach it whenever it drops, until `stop` is set.
/// Detaches before returning.
pub fn keep_attached(
    server: &Server,
    id: &str,
    backend: &dyn Backend,
    stop: &AtomicBool,
    report: Report,
) {
    let mut backoff = Duration::from_secs(1);
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        match backend.attach(server, id) {
            Ok(a) => {
                info!(%server, device = %id, port = a.port, "attached");
                report(DeviceState::Attached, Some(a.port), None);
                last_error.clear();
                backoff = Duration::from_secs(1);
                if watch(backend, &a, id, stop) {
                    match backend.detach(&a) {
                        Ok(()) => info!(%server, device = %id, port = a.port, "detached"),
                        Err(e) => {
                            warn!(%server, device = %id, error = %format!("{e:#}"), "detach failed")
                        }
                    }
                    return;
                }
                warn!(%server, device = %id, "connection lost; re-attaching");
                report(DeviceState::Retrying, None, Some("connection lost".into()));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if msg != last_error {
                    warn!(%server, device = %id, "attach failed: {msg}; retrying");
                    last_error = msg.clone();
                }
                report(DeviceState::Retrying, None, Some(msg));
            }
        }
        sleep_unless_stopped(backoff, stop);
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Returns true when asked to stop, false when the device went away.
fn watch(backend: &dyn Backend, a: &Attached, id: &str, stop: &AtomicBool) -> bool {
    loop {
        if sleep_unless_stopped(WATCH_INTERVAL, stop) {
            return true;
        }
        if !backend.is_attached(a, id) {
            return false;
        }
    }
}

pub fn sleep_unless_stopped(d: Duration, stop: &AtomicBool) -> bool {
    let step = Duration::from_millis(200);
    let mut left = d;
    while !left.is_zero() {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        let s = left.min(step);
        thread::sleep(s);
        left -= s;
    }
    stop.load(Ordering::Relaxed)
}
