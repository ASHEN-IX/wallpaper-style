//! Library surface of the control centre.
//!
//! `main.rs` owns the window; this exposes the theme and application modules so
//! integration tests can render real widgets without an OpenGL context.

pub mod app;
pub mod theme;

/// Locate the `wallpaper-style` CLI to point the systemd unit at.
///
/// The GUI must never hand its own path to the installer: the unit runs
/// `service run`, which is a CLI subcommand, so a unit pointing at the GUI fails
/// on every start. Resolution order:
///
/// 1. a CLI sitting next to this executable (the installed layout);
/// 2. `wallpaper-style` or `wallpaper-style-rs` on `PATH`;
/// 3. the conventional name, so a broken install still produces a readable unit
///    rather than the GUI's own path.
pub fn cli_binary() -> std::path::PathBuf {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        for name in ["wallpaper-style", "wallpaper-style-rs"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["wallpaper-style-rs", "wallpaper-style"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    std::path::PathBuf::from("wallpaper-style")
}
