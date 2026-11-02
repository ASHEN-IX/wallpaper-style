//! Desktop backend interface.
//!
//! Everything that differs between desktop environments sits behind these
//! traits. The KDE Plasma 6 implementation lives in `ws-plasma`; a future
//! GNOME, Xfce or Hyprland backend implements the same surface without touching
//! the domain logic, the GUI or the CLI.
//!
//! The traits are deliberately synchronous and non-generic: applying a theme
//! talks to `qdbus6` and rewrites a handful of config files, so the work is
//! short and blocking, and keeping it simple makes it testable with fakes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use ws_core::{OwnedItem, Settings, TransparencyRules};
use ws_palette::PaletteReport;

/// A feature the backend may or may not be able to provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    /// Reading the current wallpaper automatically.
    WallpaperDetection,
    /// Writing and activating a desktop colour scheme.
    ColorScheme,
    /// Generating a matching theme for Qt/Kvantum applications.
    KvantumTheme,
    /// Per-application window opacity through the compositor.
    WindowOpacity,
    /// Background-only (theme-provided) transparency.
    BackgroundTransparency,
    /// Watching for wallpaper changes without polling.
    FileWatching,
    /// A tray icon.
    TrayIcon,
}

impl Feature {
    pub const ALL: [Feature; 7] = [
        Feature::WallpaperDetection,
        Feature::ColorScheme,
        Feature::KvantumTheme,
        Feature::WindowOpacity,
        Feature::BackgroundTransparency,
        Feature::FileWatching,
        Feature::TrayIcon,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Feature::WallpaperDetection => "Automatic wallpaper detection",
            Feature::ColorScheme => "Desktop colour scheme",
            Feature::KvantumTheme => "Qt application theme (Kvantum)",
            Feature::WindowOpacity => "Per-application window opacity",
            Feature::BackgroundTransparency => "Background-only transparency",
            Feature::FileWatching => "Instant wallpaper change detection",
            Feature::TrayIcon => "Tray icon",
        }
    }
}

/// Whether a feature works, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    /// Works fully.
    Available,
    /// Works, but with a documented limitation the user should know about.
    Degraded { reason: String },
    /// Cannot be used here.
    Unavailable { reason: String },
}

impl Availability {
    pub fn is_available(&self) -> bool {
        !matches!(self, Availability::Unavailable { .. })
    }

    pub fn is_degraded(&self) -> bool {
        matches!(self, Availability::Degraded { .. })
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Availability::Available => None,
            Availability::Degraded { reason } | Availability::Unavailable { reason } => {
                Some(reason.as_str())
            }
        }
    }

    pub fn available() -> Self {
        Availability::Available
    }

    pub fn degraded(reason: impl Into<String>) -> Self {
        Availability::Degraded {
            reason: reason.into(),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Availability::Unavailable {
            reason: reason.into(),
        }
    }
}

/// The environment the backend found itself in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentReport {
    /// Desktop identifier, e.g. `KDE`.
    pub desktop: String,
    /// `wayland` or `x11`.
    pub session_type: String,
    /// Desktop version string, when known.
    pub desktop_version: Option<String>,
    /// Compositor name, when known.
    pub compositor: Option<String>,
    /// Whether this backend supports the environment at all.
    pub supported: bool,
    /// Why it is unsupported, when it is not.
    pub unsupported_reason: Option<String>,
    /// Per-feature availability.
    pub features: Vec<(Feature, Availability)>,
}

impl EnvironmentReport {
    pub fn availability(&self, feature: Feature) -> Availability {
        self.features
            .iter()
            .find(|(f, _)| *f == feature)
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| {
                Availability::unavailable("this backend does not report this feature")
            })
    }

    /// Features that are degraded or unavailable, for the GUI's status list.
    pub fn limitations(&self) -> Vec<(Feature, Availability)> {
        self.features
            .iter()
            .filter(|(_, a)| !matches!(a, Availability::Available))
            .cloned()
            .collect()
    }
}

/// A wallpaper as reported by the desktop backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedWallpaper {
    pub path: PathBuf,
    pub screen: u32,
    pub plugin: String,
    /// Set when the source is a slideshow, so the UI can explain why the accent
    /// changes on its own.
    pub is_slideshow: bool,
}

/// Result of applying a theme.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Files and settings written.
    pub changed: Vec<String>,
    /// Things that could not be done, with reasons.
    pub skipped: Vec<(String, String)>,
    /// Applications that must be restarted to pick up the change.
    pub restart_required: Vec<String>,
    /// The colour scheme name that was activated, when applicable.
    pub color_scheme: Option<String>,
}

/// What a preview shows without changing anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewReport {
    /// The palette that would be applied.
    pub palette: PaletteReport,
    /// The colour scheme file contents, for inspection.
    pub color_scheme_sample: Option<String>,
    /// Human-readable list of what applying would change.
    pub would_change: Vec<String>,
    /// Human-readable list of what applying would skip, and why.
    pub would_skip: Vec<(String, String)>,
}

/// Errors a backend can produce.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("this desktop is not supported: {0}")]
    Unsupported(String),
    #[error("no wallpaper could be detected")]
    NoWallpaper,
    #[error("the wallpaper failed to decode: {0}")]
    Image(String),
    #[error("{what} failed: {message}")]
    Command { what: String, message: String },
    #[error("cannot read or write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Other(String),
}

impl BackendError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        BackendError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn command(what: impl Into<String>, message: impl Into<String>) -> Self {
        BackendError::Command {
            what: what.into(),
            message: message.into(),
        }
    }
}

/// Reading the user's current appearance.
pub trait WallpaperSource {
    /// Report what each monitor currently shows.
    ///
    /// Implementations must not fail when a single monitor is unreadable; they
    /// report what they can and let selection policy handle the rest.
    fn detected_wallpapers(&self) -> Result<Vec<ws_core::MonitorWallpaper>, BackendError>;

    /// Resolve to the wallpaper that should drive the palette.
    fn detect(&self) -> Result<Option<DetectedWallpaper>, BackendError>;
}

/// Applying appearance changes.
pub trait ThemeApplier {
    /// Describe the environment and what works here.
    fn environment(&self) -> EnvironmentReport;

    /// Generate everything that an apply would write, without writing it.
    fn preview(&self, palette: &PaletteReport, settings: &Settings) -> PreviewReport;

    /// Write and activate. The caller is responsible for taking a snapshot
    /// first; this method records what it changed into `ownership`.
    fn apply(
        &self,
        palette: &PaletteReport,
        settings: &Settings,
        ownership: &mut ws_core::Ownership,
    ) -> Result<ApplyReport, BackendError>;

    /// Undo previously applied changes, honouring conflicts.
    fn restore(
        &self,
        snapshot: &ws_core::Snapshot,
        force: bool,
    ) -> Result<Vec<ws_core::RestoreEntry>, BackendError>;
}

/// Per-application window transparency.
pub trait TransparencyBackend {
    /// Whether window opacity can work right now. Compositing may be off, the
    /// session may be a compositor this backend cannot script, and so on.
    fn availability(&self) -> Availability;

    /// Install or update the compositor-side component. Idempotent.
    fn install(&self, rules: &TransparencyRules) -> Result<OwnedItem, BackendError>;

    /// Disable the component without deleting it.
    fn disable(&self, ownership: &mut ws_core::Ownership) -> Result<(), BackendError>;

    /// Ask the compositor to reload the component's rules.
    fn reload(&self) -> Result<(), BackendError>;

    /// Window classes currently open, for the "identify an application" tool.
    fn open_window_classes(&self) -> Result<Vec<String>, BackendError>;

    /// Whether the given app, when matched, gets whole-window or
    /// background-only transparency in this environment.
    fn transparency_kind(&self, window_class: &str) -> ws_core::TransparencyKind;
}

/// Installing files and services.
pub trait Installer {
    /// Where the application's user-level files live.
    fn install_root(&self) -> &Path;

    /// Install or update everything. Idempotent: running twice changes nothing
    /// the second time.
    fn install(&self, ownership: &mut ws_core::Ownership) -> Result<InstallReport, BackendError>;

    /// Remove everything this application installed, honouring conflicts.
    fn uninstall(&self, force: bool) -> Result<Vec<ws_core::RestoreEntry>, BackendError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InstallReport {
    pub installed: Vec<String>,
    pub unchanged: Vec<String>,
    pub warnings: Vec<String>,
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

    #[test]
    fn availability_states_are_distinguishable() {
        assert!(Availability::available().is_available());
        assert!(!Availability::available().is_degraded());
        assert!(Availability::degraded("x").is_available());
        assert!(Availability::degraded("x").is_degraded());
        assert!(!Availability::unavailable("y").is_available());
        assert_eq!(Availability::unavailable("y").reason(), Some("y"));
        assert_eq!(Availability::available().reason(), None);
    }

    #[test]
    fn every_feature_has_a_label() {
        for feature in Feature::ALL {
            assert!(!feature.label().is_empty());
        }
        // A new feature added without a label would fail this.
        assert_eq!(Feature::ALL.len(), 7);
    }

    #[test]
    fn environment_report_reports_limitations() {
        let report = EnvironmentReport {
            desktop: "KDE".into(),
            session_type: "wayland".into(),
            desktop_version: None,
            compositor: None,
            supported: true,
            unsupported_reason: None,
            features: vec![
                (Feature::ColorScheme, Availability::available()),
                (
                    Feature::WindowOpacity,
                    Availability::degraded("no compositing"),
                ),
                (Feature::TrayIcon, Availability::unavailable("not built in")),
            ],
        };
        assert!(report.availability(Feature::ColorScheme).is_available());
        assert_eq!(report.limitations().len(), 2);
        // An unreported feature must not silently look available.
        assert!(!report.availability(Feature::KvantumTheme).is_available());
    }
}
