//! Starting sibling processes: the agent (Windows only, where it runs in the
//! user's session instead of as a service) and the window from the tray.

use usbridge::paths::Paths;

#[cfg(windows)]
fn sibling(name: &str) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(
        exe.parent()?
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
    )
}

#[cfg(any(target_os = "linux", windows))]
pub fn open_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// Starts `usbridge agent` once per process if none answers.
#[cfg(windows)]
pub fn ensure_agent(paths: &Paths) {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    static TRIED: AtomicBool = AtomicBool::new(false);

    if TRIED.swap(true, Ordering::Relaxed)
        || usbridge::ipc::call(paths, usbridge::ipc::Request::Status).is_ok()
    {
        return;
    }
    let Some(exe) = sibling("usbridge") else {
        return;
    };
    let log = paths
        .endpoint
        .parent()
        .and_then(|d| {
            std::fs::create_dir_all(d).ok()?;
            std::fs::File::create(d.join("agent.log")).ok()
        })
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);
    if let Err(e) = std::process::Command::new(&exe)
        .arg("agent")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
    {
        tracing::warn!(exe = %exe.display(), error = %e, "could not start the agent");
    }
}

/// On Linux the agent is a system service.
#[cfg(not(windows))]
pub fn ensure_agent(_: &Paths) {}
