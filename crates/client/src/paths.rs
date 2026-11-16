use std::env;
use std::path::PathBuf;

/// Where the agent publishes how to reach it, and where it keeps the list of
/// devices to re-attach.
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
                endpoint: dir.join("agent.json"),
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
            endpoint: base.join("agent.json"),
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
            endpoint: base.join("agent.json"),
            state: base.join("state.json"),
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn platform() -> Self {
        Self {
            endpoint: PathBuf::from("/run/farplug/agent.json"),
            state: PathBuf::from("/var/lib/farplug/state.json"),
        }
    }
}
