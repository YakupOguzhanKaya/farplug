//! Follows the Omarchy desktop theme, when there is one: its colors.toml is
//! mapped onto egui's visuals and re-read when inotify reports a change.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui::{self, Color32, Visuals};

/// Colors the window uses beyond egui's own visuals.
#[derive(Clone, Copy)]
pub struct Palette {
    pub green: Color32,
    pub amber: Color32,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            green: Color32::from_rgb(0x2e, 0xa0, 0x43),
            amber: Color32::from_rgb(0xd2, 0x99, 0x22),
        }
    }
}

pub struct Follower {
    /// Set by the watcher when the theme files change.
    changed: Arc<AtomicBool>,
    /// What identified the applied theme, so repeated events reload once.
    applied: Option<String>,
}

impl Follower {
    pub fn new(ctx: &egui::Context) -> Self {
        let changed = Arc::new(AtomicBool::new(true));
        #[cfg(target_os = "linux")]
        if let Some(dir) = theme_dir() {
            let (c, ctx) = (changed.clone(), ctx.clone());
            watch(dir, move || {
                c.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
        #[cfg(not(target_os = "linux"))]
        let _ = ctx;
        Self {
            changed,
            applied: None,
        }
    }

    /// Applies the theme on the first call and again after it changes;
    /// returns the palette to use when it did.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Palette> {
        if !self.changed.swap(false, Ordering::Relaxed) {
            return None;
        }
        let dir = theme_dir()?;
        let stamp = stamp(&dir);
        if self.applied.as_ref() == Some(&stamp) {
            return None;
        }
        self.applied = Some(stamp);
        let text = fs::read_to_string(dir.join("colors.toml")).ok()?;
        let (visuals, palette) = from_colors(&text)?;
        ctx.set_visuals(visuals);
        Some(palette)
    }
}

/// Omarchy's staged current theme; older releases kept it under ~/.config.
fn theme_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"));
    [
        state.join("omarchy/current/theme"),
        home.join(".config/omarchy/current/theme"),
    ]
    .into_iter()
    .find(|d| d.join("colors.toml").is_file())
}

/// Calls `on_change` whenever something in the theme directory or its parent
/// changes. `omarchy theme set` may replace the directory itself, so the
/// watches are added again after every event.
#[cfg(target_os = "linux")]
fn watch(dir: PathBuf, on_change: impl Fn() + Send + 'static) {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    std::thread::spawn(move || {
        // SAFETY: inotify_init1 has no preconditions; the fd it returns is
        // owned by `file` from here on.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return;
        }
        let mut file = fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let mask = libc::IN_CLOSE_WRITE
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO;
        let dirs: Vec<CString> = [Some(dir.as_path()), dir.parent()]
            .into_iter()
            .flatten()
            .filter_map(|d| CString::new(d.as_os_str().as_bytes()).ok())
            .collect();
        let mut buf = [0u8; 4096];
        loop {
            for d in &dirs {
                // SAFETY: d is a valid C string and fd an inotify instance.
                unsafe { libc::inotify_add_watch(file.as_raw_fd(), d.as_ptr(), mask) };
            }
            match file.read(&mut buf) {
                Ok(0) => return,
                Ok(_) => on_change(),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    });
}

/// `omarchy theme set` bumps theme.generation; the file times cover edits.
fn stamp(dir: &Path) -> String {
    let generation = fs::read_to_string(dir.join("theme.generation")).unwrap_or_default();
    let modified = fs::metadata(dir.join("colors.toml"))
        .and_then(|m| m.modified())
        .map(|t| format!("{t:?}"))
        .unwrap_or_default();
    format!("{}|{}|{modified}", dir.display(), generation.trim())
}

pub fn from_colors(text: &str) -> Option<(Visuals, Palette)> {
    let table: toml::Table = toml::from_str(text).ok()?;
    let color = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| table.get(*k).and_then(|v| v.as_str()).and_then(hex))
    };
    let light = table.get("mode").and_then(|v| v.as_str()) == Some("light");
    let mut v = if light {
        Visuals::light()
    } else {
        Visuals::dark()
    };

    let bg = color(&["background"])?;
    let fg = color(&["foreground"])?;
    let raised = color(&["lighter_background"]).unwrap_or_else(|| mix(bg, fg, 0.08));
    let sunken =
        color(&["darker_background", "dark_background"]).unwrap_or_else(|| mix(bg, fg, -0.04));
    let accent = color(&["accent", "blue"]).unwrap_or(v.selection.bg_fill);
    let muted = color(&["muted"]).unwrap_or_else(|| mix(bg, fg, 0.35));

    v.panel_fill = bg;
    v.window_fill = bg;
    v.faint_bg_color = mix(bg, fg, 0.04);
    v.extreme_bg_color = sunken;
    v.code_bg_color = raised;
    v.override_text_color = Some(fg);
    v.hyperlink_color = accent;
    v.selection.bg_fill = color(&["selection_background", "selection"]).unwrap_or(accent);
    v.selection.stroke.color = color(&["selection_foreground"]).unwrap_or(fg);
    v.widgets.noninteractive.bg_fill = bg;
    v.widgets.noninteractive.weak_bg_fill = bg;
    v.widgets.noninteractive.bg_stroke.color = muted;
    v.widgets.noninteractive.fg_stroke.color = fg;
    for (w, fill) in [
        (&mut v.widgets.inactive, raised),
        (&mut v.widgets.hovered, mix(raised, accent, 0.25)),
        (&mut v.widgets.active, mix(raised, accent, 0.45)),
        (&mut v.widgets.open, raised),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.fg_stroke.color = fg;
    }
    v.widgets.hovered.bg_stroke.color = accent;
    if let Some(red) = color(&["red"]) {
        v.error_fg_color = red;
    }
    let palette = Palette {
        green: color(&["green"]).unwrap_or(Palette::default().green),
        amber: color(&["yellow", "orange"]).unwrap_or(Palette::default().amber),
    };
    v.warn_fg_color = palette.amber;
    Some((v, palette))
}

fn hex(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            byte(6)?,
        )),
        _ => None,
    }
}

/// `a` moved toward `b` by `t`; a negative `t` moves away from `b`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_colors_with_fallbacks() {
        let (v, p) = from_colors(
            r##"
            mode = "light"
            background = "#eff1f5"
            foreground = "#4c4f69"
            accent = "#1e66f5"
            green = "#40a02b"
            "##,
        )
        .unwrap();
        assert!(!v.dark_mode);
        assert_eq!(v.panel_fill, Color32::from_rgb(0xef, 0xf1, 0xf5));
        assert_eq!(
            v.override_text_color,
            Some(Color32::from_rgb(0x4c, 0x4f, 0x69))
        );
        assert_eq!(v.selection.bg_fill, Color32::from_rgb(0x1e, 0x66, 0xf5));
        assert_eq!(p.green, Color32::from_rgb(0x40, 0xa0, 0x2b));
        assert_eq!(p.amber, Palette::default().amber);

        assert!(from_colors("mode = \"dark\"\n").is_none());
        assert!(from_colors("background = \"nope\"\nforeground = \"#ffffff\"").is_none());
        assert_eq!(
            hex("#11223380"),
            Some(Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 0x80))
        );
    }
}
