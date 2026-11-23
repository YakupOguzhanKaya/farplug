// No console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod instance;
mod launch;
mod model;
mod theme;
#[cfg(any(target_os = "linux", windows))]
mod tray;
mod window;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    if std::env::args().any(|a| a == "--tray") {
        return tray();
    }
    window::run().map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(any(target_os = "linux", windows))]
fn tray() -> anyhow::Result<()> {
    tray::run()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn tray() -> anyhow::Result<()> {
    anyhow::bail!("the tray icon is not supported on this OS yet")
}
