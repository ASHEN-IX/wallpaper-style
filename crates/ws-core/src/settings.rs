//! Versioned, human-readable settings with forward migration.
//!
//! The file is TOML so a user can edit it by hand, and every document carries a
//! `schema_version` field. Loading always runs the migration chain up to
//! [`CURRENT_SCHEMA_VERSION`], so an old file is upgraded in memory, written
//! back in the new shape, and a snapshot is taken first by the caller.
//!
//! Migration also accepts the *prototype's* `settings.json` format (opacity
//! presets, `app_overrides`, `kvantum_apps`), because the product replaces an
//! existing installation and must preserve those preferences rather than
//! resetting them.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use ws_palette::{PaletteOverrides, Rgb, Variant};

/// The schema version this build reads and writes.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Application identifier, used for the desktop file, D-Bus name and paths.
pub const APP_ID: &str = "io.github.wallpaperstyle";

/// `wallpaper-style` command and service basename.
pub const APP_NAME: &str = "wallpaper-style";

/// Id of the generated KWin effect. Shared so the snapshot, the installer and the
/// generated package cannot disagree.
pub const KWIN_EFFECT_ID: &str = "wallpaper-style-transparency";

/// Opacity is clamped to this range everywhere; 1.0 means "do not touch".
pub const MIN_OPACITY: f32 = 0.20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Bumped on every incompatible change; see [`migrate`].
    ///
    /// Defaults to 0 when absent, which is how a hand-written file or one from
    /// before versioning is recognised and migrated.
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub palette: PaletteSettings,
    #[serde(default)]
    pub themes: ThemeSettings,
    #[serde(default)]
    pub transparency: TransparencySettings,
    #[serde(default)]
    pub service: ServiceSettings,
    #[serde(default)]
    pub gui: GuiSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            general: General::default(),
            palette: PaletteSettings::default(),
            themes: ThemeSettings::default(),
            transparency: TransparencySettings::default(),
            service: ServiceSettings::default(),
            gui: GuiSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    /// Apply automatically when the wallpaper changes.
    #[serde(default)]
    pub automatic: bool,
    /// The variant to apply when both are generated.
    #[serde(default)]
    pub variant: Variant,
    /// Generate matching light and dark variants.
    #[serde(default = "yes")]
    pub generate_both_variants: bool,
    /// Restart the service automatically after a settings change.
    #[serde(default = "yes")]
    pub autostart: bool,
    /// Run in the tray when the GUI is closed.
    #[serde(default)]
    pub tray_icon: bool,
    /// Poll interval for wallpaper changes, in seconds.
    #[serde(default = "default_interval")]
    pub watch_interval_seconds: u64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            automatic: false,
            variant: Variant::Dark,
            generate_both_variants: true,
            autostart: true,
            tray_icon: false,
            watch_interval_seconds: default_interval(),
        }
    }
}

fn yes() -> bool {
    true
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteSettings {
    /// A user-pinned accent. When set, hue extraction is skipped but contrast
    /// repair still applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Rgb>,
    /// Manual colour overrides applied after generation.
    #[serde(default, skip_serializing_if = "PaletteOverrides::is_empty")]
    pub overrides: PaletteOverrides,
    /// Alpha for the window background when background-only transparency is on.
    /// 255 keeps it opaque.
    #[serde(default = "opaque")]
    pub background_alpha: u8,
    /// An image to use instead of the detected wallpaper. Manual fallback for
    /// setups where detection is unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_image: Option<std::path::PathBuf>,
}

/// Hand-written rather than derived: `derive(Default)` would use `0` for
/// `background_alpha`, which means "fully transparent window backgrounds" — the
/// opposite of the documented default. A serde `default = "opaque"` attribute
/// only covers deserialisation of a file that omits the key, not a freshly
/// constructed `Settings::default()`.
impl Default for PaletteSettings {
    fn default() -> Self {
        Self {
            accent: None,
            overrides: PaletteOverrides::default(),
            background_alpha: opaque(),
            manual_image: None,
        }
    }
}

fn opaque() -> u8 {
    255
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeSettings {
    /// Write and activate a Plasma colour scheme.
    #[serde(default = "yes")]
    pub plasma_color_scheme: bool,
    /// Generate Kvantum themes when Kvantum is installed and configured.
    #[serde(default = "yes")]
    pub kvantum: bool,
    /// Generate a second glass variant of the Kvantum theme.
    #[serde(default = "yes")]
    pub kvantum_glass_variant: bool,
    /// Applications to point at the glass Kvantum theme.
    #[serde(default = "default_kvantum_apps")]
    pub kvantum_apps: Vec<String>,
    /// Enable the KWin transparency effect.
    #[serde(default)]
    pub kwin_effect: bool,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            plasma_color_scheme: true,
            kvantum: true,
            kvantum_glass_variant: true,
            kvantum_apps: default_kvantum_apps(),
            kwin_effect: false,
        }
    }
}

fn default_kvantum_apps() -> Vec<String> {
    ["dolphin", "kate", "kwrite", "ark"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransparencySettings {
    /// Master switch for per-app opacity.
    #[serde(default)]
    pub enabled: bool,
    /// Opacity for any app without a more specific rule.
    #[serde(default = "default_opacity")]
    pub default_opacity: f32,
    /// Preset-derived per-app rules, keyed by preset id.
    #[serde(default)]
    pub apps: BTreeMap<String, AppOpacity>,
    /// Extra window-class rules added by the user.
    #[serde(default)]
    pub custom: BTreeMap<String, AppOpacity>,
    /// Window classes or substrings that must never be made transparent.
    #[serde(default)]
    pub exclusions: Vec<String>,
    /// Whether to leave fullscreen windows alone. On by default: a transparent
    /// fullscreen video is a bug, not a feature.
    #[serde(default = "yes")]
    pub skip_fullscreen: bool,
}

impl Default for TransparencySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            default_opacity: default_opacity(),
            apps: BTreeMap::new(),
            custom: BTreeMap::new(),
            exclusions: Vec::new(),
            skip_fullscreen: true,
        }
    }
}

fn default_opacity() -> f32 {
    0.80
}

/// A serialisation-only mirror of [`Settings`], used to control how numbers are
/// written to the file. Deserialisation always goes through [`Settings`] itself.
#[derive(Serialize)]
struct SettingsFile {
    schema_version: u32,
    general: General,
    palette: PaletteSettings,
    themes: ThemeSettings,
    transparency: TransparencyFile,
    service: ServiceSettings,
    gui: GuiSettings,
}

#[derive(Serialize)]
struct TransparencyFile {
    enabled: bool,
    default_opacity: f64,
    apps: BTreeMap<String, AppOpacityFile>,
    custom: BTreeMap<String, AppOpacityFile>,
    exclusions: Vec<String>,
    skip_fullscreen: bool,
}

#[derive(Serialize)]
struct AppOpacityFile {
    opacity: f64,
    enabled: bool,
}

/// Convert an `f32` opacity to the `f64` a human would have written.
///
/// A direct cast reproduces the binary expansion (`0.800000011920929`), so the
/// short decimal form of the `f32` is used instead.
fn as_clean_f64(value: f32) -> f64 {
    format!("{}", round_opacity(value))
        .parse::<f64>()
        .unwrap_or(f64::from(value))
}

impl From<&Settings> for SettingsFile {
    fn from(settings: &Settings) -> Self {
        let transparency = &settings.transparency;
        let convert = |map: &BTreeMap<String, AppOpacity>| -> BTreeMap<String, AppOpacityFile> {
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        AppOpacityFile {
                            opacity: as_clean_f64(v.opacity),
                            enabled: v.enabled,
                        },
                    )
                })
                .collect()
        };
        Self {
            schema_version: settings.schema_version,
            general: settings.general.clone(),
            palette: settings.palette.clone(),
            themes: settings.themes.clone(),
            transparency: TransparencyFile {
                enabled: transparency.enabled,
                default_opacity: as_clean_f64(transparency.default_opacity),
                apps: convert(&transparency.apps),
                custom: convert(&transparency.custom),
                exclusions: transparency.exclusions.clone(),
                skip_fullscreen: transparency.skip_fullscreen,
            },
            service: settings.service.clone(),
            gui: settings.gui.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppOpacity {
    /// Opacity from 0.2 to 1.0. 1.0 leaves the window untouched.
    ///
    /// Serialised with [`opacity_serde`] so the file reads `0.8` rather than
    /// `0.800000011920929`: an `f32` holding 0.8 cannot be printed faithfully, so
    /// it is rounded to three decimals, which is finer than any UI control.
    pub opacity: f32,
    /// Whether this app participates.
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl<'de> Deserialize<'de> for AppOpacity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            opacity: f32,
            #[serde(default = "yes")]
            enabled: bool,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            opacity: round_opacity(raw.opacity),
            enabled: raw.enabled,
        })
    }
}

/// Round an opacity to three decimals, the precision the settings file shows.
///
/// This is what removes the float noise: `(0.8_f32 * 1000.0).round() / 1000.0`
/// is exactly representable enough that `toml` prints `0.8`.
pub fn round_opacity(value: f32) -> f32 {
    if !value.is_finite() {
        return value;
    }
    (value * 1000.0).round() / 1000.0
}

impl AppOpacity {
    pub fn new(opacity: f32) -> Self {
        Self {
            opacity: round_opacity(opacity),
            enabled: true,
        }
    }

    /// Clamp to the supported range. Values are validated on write and clamped
    /// on read so a hand-edited file can never break the effect.
    pub fn clamped(&self) -> Self {
        Self {
            opacity: self.opacity.clamp(MIN_OPACITY, 1.0),
            enabled: self.enabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSettings {
    /// How long to wait before retrying after a failure, in seconds.
    #[serde(default = "default_retry")]
    pub retry_seconds: u64,
    /// Log level: `error`, `warn`, `info`, `debug` or `trace`.
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            retry_seconds: default_retry(),
            log_level: default_log_level(),
        }
    }
}

fn default_retry() -> u64 {
    30
}

fn default_log_level() -> String {
    "info".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuiSettings {
    /// Show the onboarding page on first launch.
    #[serde(default = "yes")]
    pub onboarding_pending: bool,
    /// Preferred page on launch.
    #[serde(default = "default_page")]
    pub last_page: String,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            onboarding_pending: true,
            last_page: default_page(),
        }
    }
}

fn default_page() -> String {
    "overview".to_owned()
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid Wallpaper Style settings: {message}")]
    Parse {
        path: std::path::PathBuf,
        message: String,
    },
    #[error("settings schema version {found} is newer than this build supports ({supported}); refusing to overwrite it")]
    TooNew { found: u32, supported: u32 },
    #[error("invalid setting: {0}")]
    Invalid(String),
    #[error("invalid setting: {0}")]
    Validation(#[from] InvalidSetting),
}

impl Settings {
    /// Load, migrating if needed. Returns the settings plus whether they were
    /// migrated, so the caller can snapshot and rewrite.
    pub fn load(path: &Path) -> Result<(Self, bool), SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), false))
            }
            Err(source) => {
                return Err(SettingsError::Read {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        Self::from_str_at(path, &text)
    }

    /// Parse and migrate a settings document.
    pub fn from_str_at(path: &Path, text: &str) -> Result<(Self, bool), SettingsError> {
        // Peek at the version before full deserialisation so a future file
        // produces a clear error rather than a confusing unknown-field one.
        let probe: toml::Value = toml::from_str(text).map_err(|e| SettingsError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let version = probe
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .unwrap_or(0)
            .max(0) as u32;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(SettingsError::TooNew {
                found: version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }

        let mut settings: Settings = toml::from_str(text).map_err(|e| SettingsError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        let migrated = settings.schema_version < CURRENT_SCHEMA_VERSION;
        migrate(&mut settings)?;
        settings.validate()?;
        Ok((settings, migrated))
    }

    /// Write atomically. The temp file is created beside the target so a rename
    /// cannot cross filesystems.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SettingsError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let text = self.to_toml()?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut temp =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| SettingsError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        use std::io::Write;
        temp.write_all(text.as_bytes())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|source| SettingsError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        temp.persist(path).map_err(|e| SettingsError::Write {
            path: path.to_path_buf(),
            source: e.error,
        })?;
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String, SettingsError> {
        // The `toml` crate widens `f32` to `f64` and prints the exact binary
        // value, so an `f32` holding 0.8 becomes 0.800000011920929. The settings
        // file must stay human-readable, so serialise through a mirror type
        // whose opacity fields are `f64` carrying the decimal the user wrote.
        let mirror = SettingsFile::from(self);
        toml::to_string_pretty(&mirror).map_err(|e| SettingsError::Parse {
            path: std::path::PathBuf::from("<memory>"),
            message: e.to_string(),
        })
    }

    /// Reject values that would produce an unusable theme rather than clamping
    /// silently, except for opacity which is clamped deliberately.
    pub fn validate(&self) -> Result<(), InvalidSetting> {
        if self.general.watch_interval_seconds < 1 {
            return Err(InvalidSetting::new(
                "general.watch_interval_seconds",
                "must be at least 1",
            ));
        }
        if self.general.watch_interval_seconds > 3600 {
            return Err(InvalidSetting::new(
                "general.watch_interval_seconds",
                "must be at most 3600",
            ));
        }
        if self.service.retry_seconds > 3600 {
            return Err(InvalidSetting::new(
                "service.retry_seconds",
                "must be at most 3600",
            ));
        }
        if !matches!(
            self.service.log_level.as_str(),
            "error" | "warn" | "info" | "debug" | "trace"
        ) {
            return Err(InvalidSetting::new(
                "service.log_level",
                "must be one of error, warn, info, debug, trace",
            ));
        }
        for (id, app) in self
            .transparency
            .apps
            .iter()
            .chain(self.transparency.custom.iter())
        {
            if !app.opacity.is_finite() {
                return Err(InvalidSetting::new(
                    format!("transparency.{id}.opacity"),
                    "must be a number",
                ));
            }
        }
        for class in &self.transparency.exclusions {
            if class.trim().is_empty() {
                return Err(InvalidSetting::new(
                    "transparency.exclusions",
                    "must not contain empty entries",
                ));
            }
        }
        if let Some(image) = &self.palette.manual_image {
            if image.as_os_str().is_empty() {
                return Err(InvalidSetting::new(
                    "palette.manual_image",
                    "must not be empty",
                ));
            }
        }
        Ok(())
    }

    /// Normalise every value the effect consumes. Called before applying.
    pub fn normalised(&self) -> Self {
        let mut out = self.clone();
        out.transparency.default_opacity = out.transparency.default_opacity.clamp(MIN_OPACITY, 1.0);
        out.transparency.apps = out
            .transparency
            .apps
            .iter()
            .map(|(k, v)| (k.clone(), v.clamped()))
            .filter(|(_, v)| v.opacity.is_finite())
            .collect();
        out.transparency.custom = out
            .transparency
            .custom
            .iter()
            .map(|(k, v)| (k.clone(), v.clamped()))
            .filter(|(_, v)| v.opacity.is_finite())
            .collect();
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSetting {
    pub key: String,
    pub reason: String,
}

impl InvalidSetting {
    pub fn new(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for InvalidSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.key, self.reason)
    }
}

impl std::error::Error for InvalidSetting {}

/// Bring a settings document up to [`CURRENT_SCHEMA_VERSION`].
///
/// Version 0 is "no version field", which covers both a hand-written file and
/// anything produced before versioning existed.
fn migrate(settings: &mut Settings) -> Result<(), SettingsError> {
    if settings.schema_version == 0 {
        // Version 0 had no per-app master switch; every configured app was
        // active. Preserve that rather than disabling someone's setup.
        settings.schema_version = 1;
    }
    Ok(())
}

/// Preferences recovered from the Python prototype's `settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LegacyPrototypeSettings {
    #[serde(default)]
    pub opacity: LegacyOpacity,
    #[serde(default)]
    pub app_overrides: BTreeMap<String, f64>,
    #[serde(default)]
    pub kvantum: Option<bool>,
    #[serde(default)]
    pub kvantum_apps: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LegacyOpacity {
    #[serde(default)]
    pub default: Option<f64>,
    #[serde(default)]
    pub browser: Option<f64>,
    #[serde(default)]
    pub launcher: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum LegacyError {
    #[error("{path} is not readable: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a prototype settings file: {message}")]
    Parse {
        path: std::path::PathBuf,
        message: String,
    },
}

/// Import the prototype's `settings.json` into a fresh [`Settings`].
///
/// Mapping rules, chosen to preserve intent:
///
/// * `opacity.default` becomes `transparency.default_opacity`.
/// * `opacity.browser` and `opacity.launcher` become custom rules for every
///   window class the prototype applied those values to, so the same apps keep
///   the same look.
/// * `app_overrides` become custom window-class rules.
/// * `kvantum` and `kvantum_apps` map onto the theme settings.
///
/// Transparency and the KWin effect are enabled when the prototype had any
/// opacity below 1.0, because the user clearly wanted them.
pub fn import_legacy(path: &Path) -> Result<(Settings, bool), LegacyError> {
    let text = std::fs::read_to_string(path).map_err(|source| LegacyError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let legacy: LegacyPrototypeSettings =
        serde_json::from_str(&text).map_err(|e| LegacyError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
    Ok(convert_legacy(legacy))
}

/// Pure conversion, separated for testing.
pub fn convert_legacy(legacy: LegacyPrototypeSettings) -> (Settings, bool) {
    let mut settings = Settings::default();
    let mut used = false;

    let mut any_below_opaque = false;
    if let Some(default) = legacy.opacity.default {
        let clamped = (default as f32).clamp(MIN_OPACITY, 1.0);
        settings.transparency.default_opacity = clamped;
        any_below_opaque |= clamped < 1.0;
        used = true;
    }

    // The prototype treated these two groups specially in its effect template.
    if let Some(browser) = legacy.opacity.browser {
        let opacity = (browser as f32).clamp(MIN_OPACITY, 1.0);
        any_below_opaque |= opacity < 1.0;
        for class in crate::presets::BROWSER_WINDOW_CLASSES {
            settings
                .transparency
                .custom
                .insert((*class).to_owned(), AppOpacity::new(opacity));
        }
        used = true;
    }
    if let Some(launcher) = legacy.opacity.launcher {
        let opacity = (launcher as f32).clamp(MIN_OPACITY, 1.0);
        any_below_opaque |= opacity < 1.0;
        for class in crate::presets::LAUNCHER_WINDOW_CLASSES {
            settings
                .transparency
                .custom
                .insert((*class).to_owned(), AppOpacity::new(opacity));
        }
        used = true;
    }

    for (class, value) in legacy.app_overrides {
        let class = class.trim();
        if class.is_empty() || !value.is_finite() {
            continue;
        }
        let opacity = (value as f32).clamp(MIN_OPACITY, 1.0);
        any_below_opaque |= opacity < 1.0;
        settings
            .transparency
            .custom
            .insert(class.to_owned(), AppOpacity::new(opacity));
        used = true;
    }

    if let Some(kvantum) = legacy.kvantum {
        settings.themes.kvantum = kvantum;
        used = true;
    }
    if let Some(apps) = legacy.kvantum_apps {
        if !apps.is_empty() {
            settings.themes.kvantum_apps = apps
                .into_iter()
                .map(|a| a.trim().to_owned())
                .filter(|a| !a.is_empty())
                .collect();
            used = true;
        }
    }

    if any_below_opaque {
        settings.transparency.enabled = true;
        settings.themes.kwin_effect = true;
    }
    (settings, used)
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

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn defaults_are_valid_and_round_trip() {
        let settings = Settings::default();
        settings.validate().unwrap();
        let text = settings.to_toml().unwrap();
        let (back, migrated) = Settings::from_str_at(Path::new("t.toml"), &text).unwrap();
        assert_eq!(back, settings);
        assert!(!migrated);
    }

    #[test]
    fn a_missing_file_yields_defaults_without_creating_anything() {
        let dir = temp();
        let path = dir.path().join("nope.toml");
        let (settings, migrated) = Settings::load(&path).unwrap();
        assert_eq!(settings, Settings::default());
        assert!(!migrated);
        assert!(!path.exists(), "load must not write");
    }

    #[test]
    fn save_then_load_preserves_every_field() {
        let dir = temp();
        let path = dir.path().join("nested/dir/settings.toml");
        let mut settings = Settings::default();
        settings.general.automatic = true;
        settings.general.variant = Variant::Light;
        settings.palette.background_alpha = 200;
        settings.palette.accent = Some(Rgb::rgb(1, 2, 3));
        settings.transparency.enabled = true;
        settings
            .transparency
            .custom
            .insert("my-app".into(), AppOpacity::new(0.55));
        settings.transparency.exclusions.push("steam".into());
        settings.save(&path).unwrap();
        let (loaded, migrated) = Settings::load(&path).unwrap();
        assert_eq!(loaded, settings);
        assert!(!migrated);
    }

    #[test]
    fn saved_file_is_human_readable_toml_with_a_version() {
        let dir = temp();
        let path = dir.path().join("settings.toml");
        Settings::default().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("schema_version = 1"), "{text}");
        assert!(text.contains("[general]"), "{text}");
        assert!(text.contains("automatic = false"), "{text}");
    }

    #[test]
    fn an_unversioned_document_is_migrated() {
        let text = "[general]\nautomatic = true\n";
        let (settings, migrated) = Settings::from_str_at(Path::new("t.toml"), text).unwrap();
        assert!(migrated);
        assert_eq!(settings.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(settings.general.automatic);
    }

    #[test]
    fn a_future_schema_is_refused_rather_than_mangled() {
        let text = "schema_version = 99\n";
        let err = Settings::from_str_at(Path::new("t.toml"), text).unwrap_err();
        assert!(
            matches!(err, SettingsError::TooNew { found: 99, .. }),
            "{err}"
        );
    }

    #[test]
    fn unknown_keys_are_rejected_so_typos_are_visible() {
        let text = "schema_version = 1\n[nonsense]\nx = 1\n";
        let err = Settings::from_str_at(Path::new("t.toml"), text).unwrap_err();
        assert!(matches!(err, SettingsError::Parse { .. }), "{err}");
    }

    #[test]
    fn invalid_values_are_rejected_before_any_file_is_written() {
        let dir = temp();
        let path = dir.path().join("settings.toml");
        let mut settings = Settings::default();
        settings.general.watch_interval_seconds = 0;
        assert!(
            matches!(
                settings.save(&path),
                Err(SettingsError::Validation(_) | SettingsError::Invalid(_))
            ),
            "a validation failure must be reported, not written"
        );
        assert!(!path.exists());
    }

    #[test]
    fn malformed_toml_reports_the_path() {
        let err = Settings::from_str_at(Path::new("/x/settings.toml"), "this = = bad").unwrap_err();
        match err {
            SettingsError::Parse { path, .. } => assert_eq!(path, Path::new("/x/settings.toml")),
            other => panic!("unexpected {other}"),
        }
    }

    #[test]
    fn opaque_defaults_match_the_products_documented_behaviour() {
        let s = Settings::default();
        assert!(!s.general.automatic, "automatic mode must be opt-in");
        assert!(!s.transparency.enabled, "transparency must be opt-in");
        assert!(!s.themes.kwin_effect, "the effect must be opt-in");
        assert_eq!(s.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            s.themes.kvantum_apps,
            vec!["dolphin", "kate", "kwrite", "ark"]
        );
        assert!(
            s.themes.kvantum,
            "Kvantum support defaults on when available"
        );
    }

    #[test]
    fn a_fresh_settings_struct_has_an_opaque_background() {
        // Regression: PaletteSettings derived Default, which gave
        // background_alpha = 0 and made every generated theme fully transparent
        // while the file-level serde default said 255.
        let settings = Settings::default();
        assert_eq!(
            settings.palette.background_alpha, 255,
            "a default palette must not write transparent backgrounds"
        );
        assert_eq!(PaletteSettings::default().background_alpha, 255);
    }

    #[test]
    fn defaults_serialise_cleanly_without_float_noise() {
        let text = Settings::default().to_toml().unwrap();
        assert!(
            !text.contains("0.8000000"),
            "f32 values must be formatted for humans:\n{text}"
        );
        assert!(text.contains("background_alpha = 255"), "{text}");
    }

    #[test]
    fn opacity_is_clamped_into_the_supported_range() {
        let app = AppOpacity::new(0.01);
        assert_eq!(app.clamped().opacity, MIN_OPACITY);
        let app = AppOpacity::new(5.0);
        assert_eq!(app.clamped().opacity, 1.0);
    }

    #[test]
    fn non_finite_opacity_is_dropped_on_normalisation() {
        let mut settings = Settings::default();
        settings
            .transparency
            .custom
            .insert("bad".into(), AppOpacity::new(f32::NAN));
        settings
            .transparency
            .custom
            .insert("good".into(), AppOpacity::new(0.7));
        let normalised = settings.normalised();
        assert!(!normalised.transparency.custom.contains_key("bad"));
        assert_eq!(normalised.transparency.custom.len(), 1);
    }

    #[test]
    fn legacy_prototype_settings_are_preserved() {
        let legacy = LegacyPrototypeSettings {
            opacity: LegacyOpacity {
                default: Some(0.82),
                browser: Some(0.86),
                launcher: Some(0.9),
            },
            app_overrides: BTreeMap::from([("slack".to_owned(), 0.65)]),
            kvantum: Some(false),
            kvantum_apps: Some(vec!["dolphin".into(), "kate".into()]),
        };
        let (settings, used) = convert_legacy(legacy);
        assert!(used);
        assert!((settings.transparency.default_opacity - 0.82).abs() < 1e-6);
        assert_eq!(settings.transparency.custom["slack"].opacity, 0.65);
        assert_eq!(settings.transparency.custom["brave-browser"].opacity, 0.86);
        assert_eq!(settings.transparency.custom["steam"].opacity, 0.9);
        assert_eq!(settings.themes.kvantum, false);
        assert_eq!(settings.themes.kvantum_apps, vec!["dolphin", "kate"]);
        assert!(
            settings.transparency.enabled,
            "legacy opacity implies opt-in"
        );
        assert!(settings.themes.kwin_effect);
    }

    #[test]
    fn legacy_file_on_disk_is_imported() {
        let dir = temp();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"opacity":{"default":0.8,"browser":0.82,"launcher":0.85},"app_overrides":{},"kvantum":true,"kvantum_apps":["dolphin","kate","kwrite","ark"]}"#,
        )
        .unwrap();
        let (settings, used) = import_legacy(&path).unwrap();
        assert!(used);
        assert!((settings.transparency.default_opacity - 0.8).abs() < 1e-6);
        assert_eq!(settings.themes.kvantum_apps.len(), 4);
    }

    #[test]
    fn legacy_with_all_opaque_values_does_not_enable_anything() {
        let legacy = LegacyPrototypeSettings {
            opacity: LegacyOpacity {
                default: Some(1.0),
                browser: None,
                launcher: None,
            },
            ..Default::default()
        };
        let (settings, used) = convert_legacy(legacy);
        assert!(used);
        assert!(!settings.transparency.enabled);
        assert!(!settings.themes.kwin_effect);
    }

    #[test]
    fn legacy_import_rejects_garbage() {
        let dir = temp();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(matches!(
            import_legacy(&path),
            Err(LegacyError::Parse { .. })
        ));
    }

    #[test]
    fn legacy_import_of_missing_file_reports_the_path() {
        let dir = temp();
        let path = dir.path().join("absent.json");
        match import_legacy(&path) {
            Err(LegacyError::Read { path: p, .. }) => assert_eq!(p, path),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn concurrent_style_writes_are_atomic() {
        let dir = temp();
        let path = dir.path().join("settings.toml");
        let mut settings = Settings::default();
        settings.general.automatic = true;
        settings.save(&path).unwrap();
        // A second save must leave exactly one valid file, never a temp file.
        settings.general.automatic = false;
        settings.save(&path).unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["settings.toml"],
            "leftover files: {entries:?}"
        );
        let (loaded, _) = Settings::load(&path).unwrap();
        assert!(!loaded.general.automatic);
    }
}
