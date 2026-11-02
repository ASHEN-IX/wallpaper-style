//! XDG base-directory resolution.
//!
//! Every path the application touches is derived here so that a test can point
//! the whole system at a temporary tree by setting the standard variables. No
//! code may call `dirs::home_dir()` directly for anything it writes.

use std::path::{Path, PathBuf};

/// The directories this application reads and writes, resolved once at startup.
///
/// Mirrors the XDG Base Directory specification: configuration in
/// `XDG_CONFIG_HOME`, generated/installed data in `XDG_DATA_HOME`, mutable state
/// (snapshots, caches of what was applied) in `XDG_STATE_HOME`, and runtime
/// files in `XDG_RUNTIME_DIR`. Nothing is ever written to the home directory
/// directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/wallpaper-style`
    pub config_dir: PathBuf,
    /// `$XDG_DATA_HOME/wallpaper-style` (themes, KWin effect package)
    pub data_dir: PathBuf,
    /// `$XDG_STATE_HOME/wallpaper-style` (snapshots, last-applied record)
    pub state_dir: PathBuf,
    /// `$XDG_CACHE_HOME/wallpaper-style` (decoded thumbnails for the GUI)
    pub cache_dir: PathBuf,
    /// `$XDG_CONFIG_HOME` itself, needed for files KDE owns (`kdeglobals`, ...)
    pub xdg_config: PathBuf,
    /// `$XDG_DATA_HOME` itself
    pub xdg_data: PathBuf,
    /// `$XDG_STATE_HOME` itself
    pub xdg_state: PathBuf,
    /// `$XDG_RUNTIME_DIR`, when present.
    pub runtime_dir: Option<PathBuf>,
    /// The user's home directory, used only for `~` display and systemd `%h`.
    pub home: PathBuf,
}

impl Paths {
    /// Resolve from the environment, creating nothing.
    pub fn from_env() -> Result<Self, PathError> {
        Self::from_lookup(|key| std::env::var_os(key).map(PathBuf::from))
    }

    /// Resolve using an arbitrary lookup function. Tests use this to build a
    /// sandboxed tree; the application uses [`Paths::from_env`].
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<PathBuf>) -> Result<Self, PathError> {
        let home = lookup("HOME").ok_or(PathError::MissingHome)?;
        if !home.is_absolute() {
            return Err(PathError::Relative {
                key: "HOME",
                value: home,
            });
        }

        let absolute = |key: &str, fallback: PathBuf| -> Result<PathBuf, PathError> {
            match lookup(key) {
                None => Ok(fallback),
                Some(value) => {
                    if value.is_absolute() {
                        Ok(value)
                    } else {
                        Err(PathError::Relative {
                            key: key_name(key),
                            value,
                        })
                    }
                }
            }
        };

        let xdg_config = absolute("XDG_CONFIG_HOME", home.join(".config"))?;
        let xdg_data = absolute("XDG_DATA_HOME", home.join(".local/share"))?;
        let xdg_state = absolute("XDG_STATE_HOME", home.join(".local/state"))?;
        let xdg_cache = absolute("XDG_CACHE_HOME", home.join(".cache"))?;
        // XDG_RUNTIME_DIR is optional and set only inside a user session.
        let runtime_dir = lookup("XDG_RUNTIME_DIR").filter(|p| p.is_absolute());

        Ok(Self {
            config_dir: xdg_config.join("wallpaper-style"),
            data_dir: xdg_data.join("wallpaper-style"),
            state_dir: xdg_state.join("wallpaper-style"),
            cache_dir: xdg_cache.join("wallpaper-style"),
            xdg_config,
            xdg_data,
            xdg_state,
            runtime_dir,
            home,
        })
    }

    /// `settings.toml` under the config directory.
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    /// Versioned snapshots of everything the application changed.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.state_dir.join("snapshots")
    }

    /// The manifest listing exactly which files and settings the app owns.
    pub fn ownership_file(&self) -> PathBuf {
        self.state_dir.join("ownership.json")
    }

    /// Record of the most recently applied theme, used to detect drift.
    pub fn last_applied_file(&self) -> PathBuf {
        self.state_dir.join("last-applied.json")
    }

    /// `systemd --user` unit directory.
    pub fn systemd_user_dir(&self) -> PathBuf {
        self.xdg_config.join("systemd/user")
    }

    /// KDE colour schemes live in `$XDG_DATA_HOME/color-schemes`.
    pub fn kde_color_schemes_dir(&self) -> PathBuf {
        self.xdg_data.join("color-schemes")
    }

    /// Per-user KWin effects live in `$XDG_DATA_HOME/kwin/effects`.
    pub fn kwin_effects_dir(&self) -> PathBuf {
        self.xdg_data.join("kwin/effects")
    }

    /// Per-user Kvantum themes live in `$XDG_CONFIG_HOME/Kvantum`.
    pub fn kvantum_dir(&self) -> PathBuf {
        self.xdg_config.join("Kvantum")
    }

    /// `kdeglobals`, which records the active colour scheme name.
    pub fn kdeglobals(&self) -> PathBuf {
        self.xdg_config.join("kdeglobals")
    }

    /// `kwinrc`, which records which effects are enabled.
    pub fn kwinrc(&self) -> PathBuf {
        self.xdg_config.join("kwinrc")
    }

    /// `plasma-org.kde.plasma.desktop-appletsrc`, the Plasma wallpaper store.
    pub fn plasma_appletsrc(&self) -> PathBuf {
        self.xdg_config
            .join("plasma-org.kde.plasma.desktop-appletsrc")
    }

    /// `$XDG_DATA_DIRS`, defaulting to the specification's fallback list.
    pub fn data_dirs(&self) -> Vec<PathBuf> {
        match std::env::var_os("XDG_DATA_DIRS") {
            Some(value) => {
                let dirs: Vec<PathBuf> = std::env::split_paths(&value)
                    .filter(|p| p.is_absolute())
                    .collect();
                if dirs.is_empty() {
                    default_data_dirs()
                } else {
                    dirs
                }
            }
            None => default_data_dirs(),
        }
    }

    /// `$XDG_CONFIG_DIRS`, defaulting to `/etc/xdg`.
    pub fn config_dirs(&self) -> Vec<PathBuf> {
        match std::env::var_os("XDG_CONFIG_DIRS") {
            Some(value) => {
                let dirs: Vec<PathBuf> = std::env::split_paths(&value)
                    .filter(|p| p.is_absolute())
                    .collect();
                if dirs.is_empty() {
                    vec![PathBuf::from("/etc/xdg")]
                } else {
                    dirs
                }
            }
            None => vec![PathBuf::from("/etc/xdg")],
        }
    }

    /// Replace a leading home directory with `~` for display in the UI and logs.
    /// This never affects what is written to disk.
    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }
}

/// Map a lookup key to the static name used in error messages.
fn key_name(key: &str) -> &'static str {
    match key {
        "XDG_CONFIG_HOME" => "XDG_CONFIG_HOME",
        "XDG_DATA_HOME" => "XDG_DATA_HOME",
        "XDG_STATE_HOME" => "XDG_STATE_HOME",
        "XDG_CACHE_HOME" => "XDG_CACHE_HOME",
        "HOME" => "HOME",
        _ => "an XDG variable",
    }
}

fn default_data_dirs() -> Vec<PathBuf> {
    ["/usr/local/share", "/usr/share"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("HOME is not set; cannot resolve user directories")]
    MissingHome,
    #[error("{key} must be an absolute path, but is {value:?}")]
    Relative { key: &'static str, value: PathBuf },
}

#[cfg(test)]
mod tests {
    // Test modules use deliberate idioms clippy flags by default: exhaustive
    // `other => panic!()` guards so a new variant cannot pass silently, and
    // `assert!(x.is_empty())` for readability.
    #![allow(
        clippy::single_match_else,
        clippy::wildcard_in_or_patterns,
        clippy::bool_assert_comparison,
        clippy::struct_excessive_bools,
        clippy::match_same_arms,
        clippy::needless_borrow,
        clippy::map_unwrap_or,
        clippy::unnecessary_wraps,
        clippy::redundant_closure,
        clippy::unused_unit,
        clippy::items_after_statements,
        clippy::needless_borrows_for_generic_args,
        clippy::ptr_arg
    )]
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let map: HashMap<String, PathBuf> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), PathBuf::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn defaults_follow_the_xdg_specification() {
        let paths = Paths::from_lookup(lookup(&[("HOME", "/home/someone")])).unwrap();
        assert_eq!(
            paths.config_dir,
            PathBuf::from("/home/someone/.config/wallpaper-style")
        );
        assert_eq!(
            paths.data_dir,
            PathBuf::from("/home/someone/.local/share/wallpaper-style")
        );
        assert_eq!(
            paths.state_dir,
            PathBuf::from("/home/someone/.local/state/wallpaper-style")
        );
        assert_eq!(
            paths.cache_dir,
            PathBuf::from("/home/someone/.cache/wallpaper-style")
        );
        assert_eq!(
            paths.kdeglobals(),
            PathBuf::from("/home/someone/.config/kdeglobals")
        );
        assert_eq!(
            paths.kde_color_schemes_dir(),
            PathBuf::from("/home/someone/.local/share/color-schemes")
        );
    }

    #[test]
    fn explicit_xdg_variables_are_honoured() {
        let paths = Paths::from_lookup(lookup(&[
            ("HOME", "/home/someone"),
            ("XDG_CONFIG_HOME", "/tmp/cfg"),
            ("XDG_DATA_HOME", "/tmp/data"),
            ("XDG_STATE_HOME", "/tmp/state"),
            ("XDG_CACHE_HOME", "/tmp/cache"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]))
        .unwrap();
        assert_eq!(
            paths.settings_file(),
            PathBuf::from("/tmp/cfg/wallpaper-style/settings.toml")
        );
        assert_eq!(paths.kvantum_dir(), PathBuf::from("/tmp/cfg/Kvantum"));
        assert_eq!(paths.runtime_dir, Some(PathBuf::from("/run/user/1000")));
    }

    #[test]
    fn relative_xdg_paths_are_rejected_not_silently_used() {
        let err = Paths::from_lookup(lookup(&[
            ("HOME", "/home/x"),
            ("XDG_CONFIG_HOME", "relative"),
        ]))
        .unwrap_err();
        assert!(
            matches!(
                err,
                PathError::Relative {
                    key: "XDG_CONFIG_HOME",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn missing_home_is_an_error_not_a_guess() {
        let err = Paths::from_lookup(lookup(&[])).unwrap_err();
        assert!(matches!(err, PathError::MissingHome));
    }

    #[test]
    fn display_shortens_only_the_home_prefix() {
        let paths = Paths::from_lookup(lookup(&[("HOME", "/home/someone")])).unwrap();
        assert_eq!(
            paths.display(Path::new("/home/someone/.config/x")),
            "~/.config/x"
        );
        // A sibling that merely shares a prefix must not be shortened.
        assert_eq!(
            paths.display(Path::new("/home/someoneelse/file")),
            "/home/someoneelse/file"
        );
    }
}
