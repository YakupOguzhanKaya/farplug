//! The usbridge client: talking to servers, keeping devices attached, and the
//! local agent that the CLI and GUI control.

pub mod agent;
pub mod backend;
pub mod ipc;
#[cfg(target_os = "linux")]
mod linux;
pub mod paths;
pub mod server;
pub mod supervisor;
#[cfg(target_os = "linux")]
mod vhci;
mod winabi;
#[cfg(windows)]
mod windows;

pub use backend::{Attached, Backend};
pub use server::Server;
