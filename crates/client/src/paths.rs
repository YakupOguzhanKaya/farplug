use std::env;
use std::path::PathBuf;

/// The agent's socket on Linux, or elsewhere the file where it publishes its
/// port and token.
#[cfg(target_os = "linux")]
const ENDPOINT: &str = "agent.sock";
#[cfg(not(target_os = "linux"))]
const ENDPOINT: &str = "agent.json";

/// How to reach the agent, and where it keeps the list of devices to
/// re-attach.
#[derive(Debug, Clone)]
pub struct Paths {
    pub endpoint: PathBuf,
    pub state: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        if let Some(dir) = env::var_os("FARPLUG_AGENT_DIR") {
            let dir = PathBuf::from(dir);
            return Self {
                endpoint: dir.join(ENDPOINT),
                state: dir.join("state.json"),
            };
        }
        Self::platform()
    }
}

impl Paths {
    #[cfg(windows)]
    fn platform() -> Self {
        // Per-user: the agent runs in the user's session, and LOCALAPPDATA is
        // readable only by that user.
        let base = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(env::temp_dir)
            .join("farplug");
        Self {
            endpoint: base.join(ENDPOINT),
            state: base.join("state.json"),
        }
    }

    /// Per-user, like Windows: the agent runs as the user's LaunchAgent.
    #[cfg(target_os = "macos")]
    fn platform() -> Self {
        let base = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(env::temp_dir)
            .join("Library/Application Support/farplug");
        Self {
            endpoint: base.join(ENDPOINT),
            state: base.join("state.json"),
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn platform() -> Self {
        Self {
            endpoint: PathBuf::from("/run/farplug").join(ENDPOINT),
            state: PathBuf::from("/var/lib/farplug/state.json"),
        }
    }
}

/// Where stable links to serial ports are made, on systems where farplug
/// offers devices as serial ports: a short path without spaces, so it is
/// easy to type into other programs.
#[cfg(unix)]
pub fn serial_dir() -> PathBuf {
    if let Some(dir) = env::var_os("FARPLUG_AGENT_DIR") {
        return PathBuf::from(dir).join("serial");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join(".farplug/serial")
}
