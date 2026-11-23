//! The farplug client: talking to servers, keeping devices attached, and the
//! local agent that the CLI and GUI control.

pub mod agent;
#[cfg(target_os = "linux")]
mod auth;
pub mod backend;
pub mod ipc;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod mdns;
pub mod paths;
#[cfg(unix)]
pub mod serial;
pub mod server;
pub mod supervisor;
#[cfg(unix)]
mod user;
pub mod userhost;
#[cfg(target_os = "linux")]
mod vhci;
mod winabi;
#[cfg(windows)]
mod windows;

pub use backend::{Attached, Backend};
pub use server::Server;
