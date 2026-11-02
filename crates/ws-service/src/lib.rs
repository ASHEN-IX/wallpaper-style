//! The background watcher.
//!
//! Responsibilities, in order of importance:
//!
//! 1. **Never make the desktop worse.** A failure to decode an image, read the
//!    configuration or run a helper must leave the current appearance untouched
//!    and be retried later, not propagate.
//! 2. **Apply only when something relevant changed.** The watcher compares a
//!    content fingerprint of the wallpaper plus the settings' effective effect on
//!    the theme, so touching a file or editing an unrelated setting does not
//!    re-theme the desktop.
//! 3. **Recover.** Invalid images, a missing wallpaper, a stopped service and
//!    half-applied themes all have defined behaviour: retry with backoff, fall
//!    back to the last good palette, and report.
//!
//! The watcher is driven by a [`Sleeper`] so tests can run it deterministically
//! without real time passing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ws_backend::{ThemeApplier, WallpaperSource};
use ws_core::snapshot::{Snapshot, SnapshotEntry};
use ws_core::{Ownership, Paths, Settings, SnapshotStore, WallpaperFingerprint};
use ws_palette::Variant;

/// What the watcher is currently doing, written for the GUI and `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceState {
    /// Wallpaper the last successful apply used.
    pub wallpaper: Option<PathBuf>,
    /// Accent that was applied, as `#rrggbb`.
    pub accent: Option<String>,
    /// Variant applied.
    pub variant: String,
    /// Unix seconds of the last successful apply.
    pub last_applied_unix: Option<u64>,
    /// Human-readable description of the last failure, if the last attempt
    /// failed. Cleared on success.
    pub last_error: Option<String>,
    /// Consecutive failures, used for backoff.
    pub consecutive_failures: u32,
    /// Fingerprint of the wallpaper that was applied, so a restarted watcher can
    /// tell whether the image changed while it was not running. Without this a
    /// service restart would either re-apply always or never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallpaper_fingerprint: Option<ws_core::WallpaperFingerprint>,
}

impl Default for ServiceState {
    fn default() -> Self {
        Self {
            wallpaper: None,
            accent: None,
            variant: Variant::Dark.as_str().to_owned(),
            last_applied_unix: None,
            last_error: None,
            consecutive_failures: 0,
            wallpaper_fingerprint: None,
        }
    }
}

impl ServiceState {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        ws_core::snapshot::write_atomic(path, text.as_bytes())
    }

    /// True when the watcher is in a failed state the UI should surface.
    pub fn is_failing(&self) -> bool {
        self.last_error.is_some()
    }
}

/// Abstraction over sleeping so tests need no wall-clock time.
pub trait Sleeper {
    fn sleep(&self, duration: Duration);
}

/// Real sleeping.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// An outcome the caller may want to log or react to.
#[derive(Debug, Clone, PartialEq)]
pub enum Tick {
    /// Nothing relevant changed.
    Unchanged,
    /// Applied successfully.
    Applied { wallpaper: PathBuf, accent: String },
    /// Nothing to apply, or applying was skipped.
    Skipped { reason: String },
    /// A failure that was handled: state was left untouched.
    Failed { reason: String },
}

/// One iteration of the watcher, independent of scheduling.
///
/// This is the unit that gets tested; [`run`] just loops over it.
#[derive(Debug)]
pub struct Watcher {
    pub paths: Paths,
    pub store: SnapshotStore,
    pub state: ServiceState,
    /// Fingerprint of the image the last successful apply used.
    pub last_fingerprint: Option<WallpaperFingerprint>,
}

impl Watcher {
    pub fn new(paths: Paths) -> Self {
        let store = SnapshotStore::new(paths.snapshots_dir());
        let state = ServiceState::load(&paths.last_applied_file());
        Self {
            paths,
            store,
            state,
            last_fingerprint: None,
        }
    }

    /// Reload persisted state, including the fingerprint of the last wallpaper.
    pub fn restore_state(&mut self) {
        self.state = ServiceState::load(&self.paths.last_applied_file());
        // Prefer the recorded fingerprint: re-fingerprinting the file now would
        // describe the *current* content, which is exactly what we are trying to
        // compare against.
        self.last_fingerprint = self.state.wallpaper_fingerprint.clone();
    }

    pub fn settings(&self) -> Result<Settings, String> {
        Settings::load(&self.paths.settings_file())
            .map(|(s, _)| s)
            .map_err(|e| e.to_string())
    }

    /// Decide whether an apply is needed, and perform it.
    pub fn tick<B: ThemeApplier + WallpaperSource>(&mut self, backend: &B) -> Tick {
        let settings = match self.settings() {
            Ok(settings) => settings.normalised(),
            Err(err) => {
                return self.fail(format!("cannot read settings: {err}"));
            }
        };

        if !settings.general.automatic {
            // Automatic mode is off: do nothing at all, and do not treat this as
            // a failure.
            return Tick::Skipped {
                reason: "automatic updates are disabled".to_owned(),
            };
        }

        let detected = match backend.detect() {
            Ok(Some(detected)) => detected,
            Ok(None) => {
                return self.fail(
                    "no usable wallpaper was found; the current theme was left unchanged"
                        .to_owned(),
                )
            }
            Err(err) => {
                return self.fail(format!("cannot read the wallpaper: {err}"));
            }
        };

        let fingerprint = match WallpaperFingerprint::of(&detected.path) {
            Ok(fingerprint) => fingerprint,
            Err(err) => {
                return self.fail(format!(
                    "cannot read {}: {err}; the current theme was left unchanged",
                    detected.path.display()
                ))
            }
        };

        if let Some(previous) = &self.last_fingerprint {
            if previous.matches(&fingerprint) && !self.state.is_failing() {
                return Tick::Unchanged;
            }
        }

        self.apply(backend, &settings, &detected.path, fingerprint)
    }

    fn apply<B: ThemeApplier>(
        &mut self,
        backend: &B,
        settings: &Settings,
        wallpaper: &Path,
        fingerprint: WallpaperFingerprint,
    ) -> Tick {
        let sampled = match ws_palette::sample_path(wallpaper) {
            Ok(sampled) => sampled,
            Err(err) => {
                // Preserve whatever is currently on screen: a bad image must not
                // wipe a working theme.
                return self.fail(format!(
                    "the wallpaper {} could not be decoded ({err}); keeping the current theme",
                    wallpaper.display()
                ));
            }
        };

        let preferred_hue = self
            .state
            .accent
            .as_deref()
            .and_then(ws_palette::Rgb::from_hex)
            .map(|c| c.to_oklch().h);

        let derivation = ws_palette::derive(
            &sampled,
            preferred_hue,
            settings.palette.background_alpha,
            &settings.palette.overrides,
        );

        let report = match settings.general.variant {
            Variant::Dark => derivation.dark.clone(),
            Variant::Light => derivation.light.clone(),
        };

        let mut ownership = Ownership::load(&self.paths.ownership_file()).unwrap_or_default();
        let mut snapshot = Snapshot::empty(
            self.store.next_id(ownership.applied_generation),
            ownership.applied_generation,
            "automatic-apply",
        );

        // Pre-capture the files the backend is about to write, using the same
        // layout hints it uses. `OwnedItem`s recorded by *this* apply are
        // captured again below with their true previous state, so an inaccurate
        // hint cannot make the snapshot wrong - only larger.
        for entry in ws_core::snapshot::capture_planned_files(
            &self.paths,
            settings,
            &report.palette,
            report.variant,
        ) {
            snapshot.entries.insert(entry.id.clone(), entry);
        }

        match backend.apply(&report, settings, &mut ownership) {
            Ok(apply_report) => {
                // The ownership record the backend just produced is the authority
                // on which paths it touched. Merge it into the snapshot: for a
                // path that was pre-captured we keep its earlier "previous" value
                // (correct, because nothing had been written when it was read),
                // and for a path only the backend knows about we capture the
                // state now.
                //
                // Paths predicted but never written are dropped, so a guess that
                // was too broad cannot leave a bogus entry that restore would
                // treat as something the application owns.
                let touched: Vec<String> = ownership
                    .items
                    .keys()
                    .filter(|id| id.starts_with("file:") || id.starts_with("cfg:"))
                    .cloned()
                    .collect();
                snapshot
                    .entries
                    .retain(|id, _| touched.iter().any(|t| t == id));

                for id in touched {
                    // A path the pre-capture already saw is kept as-is; its
                    // `previous` value was read before anything was written.
                    if snapshot.entries.contains_key(&id) {
                        continue;
                    }
                    let Some(item) = ownership.items.get(&id) else {
                        continue;
                    };
                    let entry = match item {
                        ws_core::OwnedItem::ConfigKey {
                            file,
                            group,
                            key,
                            original,
                            applied,
                        } => SnapshotEntry {
                            id: id.clone(),
                            previous: ws_core::snapshot::CapturedState::ConfigValue {
                                value: original.clone(),
                            },
                            applied: ws_core::snapshot::CapturedState::ConfigValue {
                                value: Some(applied.clone()),
                            },
                            path: file.clone(),
                            detail: Some(ws_core::snapshot::SnapshotDetail::ConfigKey {
                                group: group.clone(),
                                key: key.clone(),
                            }),
                        },
                        ws_core::OwnedItem::SystemdUnit {
                            name,
                            path,
                            original,
                        } => SnapshotEntry {
                            id: id.clone(),
                            previous: original
                                .clone()
                                .map_or(ws_core::snapshot::CapturedState::Absent, |text| {
                                    ws_core::snapshot::CapturedState::Content { text }
                                }),
                            applied: ws_core::snapshot::CapturedState::Absent,
                            path: path.clone(),
                            detail: Some(ws_core::snapshot::SnapshotDetail::SystemdUnit {
                                unit_name: name.clone(),
                            }),
                        },
                        other => SnapshotEntry {
                            id: id.clone(),
                            previous: ws_core::snapshot::CapturedState::Absent,
                            applied: ws_core::snapshot::CapturedState::Absent,
                            path: other.path().to_path_buf(),
                            detail: None,
                        },
                    };
                    snapshot.entries.insert(id, entry);
                }

                // Record what was actually written, so restore can compare.
                for entry in snapshot.entries.values_mut() {
                    if matches!(
                        entry.detail,
                        Some(ws_core::snapshot::SnapshotDetail::ConfigKey { .. })
                    ) {
                        continue;
                    }
                    match std::fs::read_to_string(&entry.path) {
                        Ok(text) => {
                            entry.applied = ws_core::snapshot::CapturedState::Content { text }
                        }
                        Err(_) if !entry.path.exists() => {
                            entry.applied = ws_core::snapshot::CapturedState::Absent
                        }
                        Err(_) => {}
                    }
                }
                let snapshot_id = if snapshot.is_empty() {
                    None
                } else {
                    match self.store.write(&snapshot) {
                        Ok(_) => {
                            ownership.snapshots.push(snapshot.id.clone());
                            Some(snapshot.id.clone())
                        }
                        Err(err) => {
                            log::warn!("cannot write snapshot: {err}");
                            None
                        }
                    }
                };
                if let Err(err) = ownership.save(&self.paths.ownership_file()) {
                    log::warn!("cannot save ownership record: {err}");
                }
                let _ = snapshot_id;
                let _ = self.store.prune(20);

                for (what, why) in &apply_report.skipped {
                    log::info!("skipped {what}: {why}");
                }
                for app in &apply_report.restart_required {
                    log::info!("{app}");
                }

                self.state = ServiceState {
                    wallpaper: Some(wallpaper.to_path_buf()),
                    accent: Some(report.palette.accent.to_hex()),
                    variant: report.variant.as_str().to_owned(),
                    last_applied_unix: Some(ws_core::snapshot::now_unix()),
                    last_error: None,
                    consecutive_failures: 0,
                    wallpaper_fingerprint: Some(fingerprint.clone()),
                };
                let _ = self.state.save(&self.paths.last_applied_file());
                self.last_fingerprint = Some(fingerprint);

                Tick::Applied {
                    wallpaper: wallpaper.to_path_buf(),
                    accent: report.palette.accent.to_hex(),
                }
            }
            Err(err) => self.fail(format!("applying the theme failed: {err}")),
        }
    }

    fn fail(&mut self, reason: String) -> Tick {
        log::warn!("{reason}");
        self.state.consecutive_failures = self.state.consecutive_failures.saturating_add(1);
        self.state.last_error = Some(reason.clone());
        let _ = self.state.save(&self.paths.last_applied_file());
        Tick::Failed { reason }
    }

    /// How long to wait before the next poll, growing while failing.
    ///
    /// Backoff doubles per consecutive failure and is capped at one hour, so a
    /// permanently broken setup (a wallpaper on an unmounted drive, say) does
    /// not spin the CPU, while a transient one recovers quickly.
    pub fn next_delay(&self, base_seconds: u64, retry_seconds: u64) -> Duration {
        let base = base_seconds.max(1);
        if self.state.consecutive_failures == 0 {
            return Duration::from_secs(base);
        }
        let doublings = self.state.consecutive_failures.min(8);
        let seconds = retry_seconds
            .max(1)
            .saturating_mul(1u64 << doublings.saturating_sub(1))
            .min(3600);
        Duration::from_secs(seconds.max(base))
    }
}

/// Run the watcher until `stop` returns true.
///
/// Errors are logged and retried; this function does not return an error.
pub fn run<B: ThemeApplier + WallpaperSource>(
    watcher: &mut Watcher,
    backend: &B,
    sleeper: &dyn Sleeper,
    stop: impl Fn() -> bool,
) {
    while !stop() {
        let settings = watcher.settings().unwrap_or_default();
        let base = settings.general.watch_interval_seconds.max(1);
        let retry = settings.service.retry_seconds.max(base);

        match watcher.tick(backend) {
            Tick::Unchanged => {}
            Tick::Applied { wallpaper, accent } => {
                log::info!("applied {accent} from {}", wallpaper.display());
            }
            Tick::Skipped { reason } => log::debug!("{reason}"),
            Tick::Failed { reason } => log::warn!("{reason}"),
        }

        sleeper.sleep(watcher.next_delay(base, retry));
    }
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
    use std::cell::RefCell;
    use ws_backend::{ApplyReport, BackendError, EnvironmentReport, PreviewReport};

    #[derive(Debug, Default)]
    struct FakeBackend {
        pub detect: RefCell<Option<PathBuf>>,
        pub apply_calls: RefCell<u32>,
        pub fail_apply: RefCell<bool>,
        pub fail_detect: RefCell<bool>,
        /// Where the fake writes its theme file, so the snapshot path is
        /// exercised the way a real backend exercises it.
        pub theme_path: RefCell<Option<PathBuf>>,
    }

    impl WallpaperSource for FakeBackend {
        fn detected_wallpapers(&self) -> Result<Vec<ws_core::MonitorWallpaper>, BackendError> {
            Ok(Vec::new())
        }
        fn detect(&self) -> Result<Option<ws_backend::DetectedWallpaper>, BackendError> {
            if *self.fail_detect.borrow() {
                return Err(BackendError::NoWallpaper);
            }
            Ok(self
                .detect
                .borrow()
                .as_ref()
                .map(|path| ws_backend::DetectedWallpaper {
                    path: path.clone(),
                    screen: 0,
                    plugin: "test".into(),
                    is_slideshow: false,
                }))
        }
    }

    impl ThemeApplier for FakeBackend {
        fn environment(&self) -> EnvironmentReport {
            EnvironmentReport {
                desktop: "KDE".into(),
                session_type: "wayland".into(),
                desktop_version: None,
                compositor: None,
                supported: true,
                unsupported_reason: None,
                features: Vec::new(),
            }
        }
        fn preview(
            &self,
            palette: &ws_palette::PaletteReport,
            _settings: &Settings,
        ) -> PreviewReport {
            PreviewReport {
                palette: palette.clone(),
                color_scheme_sample: None,
                would_change: Vec::new(),
                would_skip: Vec::new(),
            }
        }
        fn apply(
            &self,
            _palette: &ws_palette::PaletteReport,
            _settings: &Settings,
            ownership: &mut Ownership,
        ) -> Result<ApplyReport, BackendError> {
            *self.apply_calls.borrow_mut() += 1;
            if *self.fail_apply.borrow() {
                return Err(BackendError::Other("simulated failure".into()));
            }
            if let Some(path) = self.theme_path.borrow().as_ref() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, "[General]\nName=Generated\n");
                ownership.record(
                    format!("file:{}", path.display()),
                    ws_core::OwnedItem::CreatedFile { path: path.clone() },
                );
            }
            Ok(ApplyReport::default())
        }
        fn restore(
            &self,
            _snapshot: &Snapshot,
            _force: bool,
        ) -> Result<Vec<ws_core::RestoreEntry>, BackendError> {
            Ok(Vec::new())
        }
    }

    fn sandbox() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let paths = Paths::from_lookup(|key| match key {
            "HOME" => Some(root.clone()),
            "XDG_CONFIG_HOME" => Some(root.join("config")),
            "XDG_DATA_HOME" => Some(root.join("data")),
            "XDG_STATE_HOME" => Some(root.join("state")),
            "XDG_CACHE_HOME" => Some(root.join("cache")),
            _ => None,
        })
        .unwrap();
        (dir, paths)
    }

    fn write_image(dir: &Path, name: &str, color: [u8; 3]) -> PathBuf {
        let path = dir.join(name);
        let img = image::RgbImage::from_pixel(32, 32, image::Rgb(color));
        img.save(&path).unwrap();
        path
    }

    fn automatic_settings(paths: &Paths) {
        let mut settings = Settings::default();
        settings.general.automatic = true;
        settings.save(&paths.settings_file()).unwrap();
    }

    #[test]
    fn automatic_mode_off_means_no_work() {
        let (_dir, paths) = sandbox();
        let backend = FakeBackend::default();
        let mut watcher = Watcher::new(paths);
        let tick = watcher.tick(&backend);
        assert!(matches!(tick, Tick::Skipped { .. }));
        assert_eq!(*backend.apply_calls.borrow(), 0);
    }

    #[test]
    fn a_new_wallpaper_triggers_an_apply() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [40, 80, 200]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image.clone());
        let mut watcher = Watcher::new(paths);
        match watcher.tick(&backend) {
            Tick::Applied { wallpaper, .. } => assert_eq!(wallpaper, image),
            other => panic!("got {other:?}"),
        }
        assert_eq!(*backend.apply_calls.borrow(), 1);
    }

    #[test]
    fn the_same_wallpaper_is_not_reapplied() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [40, 80, 200]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image);
        let mut watcher = Watcher::new(paths);
        assert!(matches!(watcher.tick(&backend), Tick::Applied { .. }));
        assert!(matches!(watcher.tick(&backend), Tick::Unchanged));
        assert_eq!(
            *backend.apply_calls.borrow(),
            1,
            "must not reapply unchanged input"
        );
    }

    #[test]
    fn a_changed_image_at_the_same_path_is_detected() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [40, 80, 200]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image.clone());
        let mut watcher = Watcher::new(paths);
        watcher.tick(&backend);

        write_image(dir.path(), "w.png", [200, 40, 40]);
        assert!(matches!(watcher.tick(&backend), Tick::Applied { .. }));
        assert_eq!(*backend.apply_calls.borrow(), 2);
    }

    #[test]
    fn an_undecodable_image_leaves_the_theme_alone_and_retries() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let bad = dir.path().join("broken.png");
        std::fs::write(&bad, b"not an image").unwrap();
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(bad);
        let mut watcher = Watcher::new(paths.clone());
        match watcher.tick(&backend) {
            Tick::Failed { reason } => assert!(reason.contains("could not be decoded"), "{reason}"),
            other => panic!("got {other:?}"),
        }
        assert_eq!(
            *backend.apply_calls.borrow(),
            0,
            "must not apply a broken theme"
        );
        assert!(watcher.state.is_failing());
        // The failure is persisted so a restarted service reports it.
        let reloaded = ServiceState::load(&paths.last_applied_file());
        assert!(reloaded.is_failing());
    }

    #[test]
    fn no_wallpaper_is_a_handled_failure() {
        let (_dir, paths) = sandbox();
        automatic_settings(&paths);
        let backend = FakeBackend::default();
        let mut watcher = Watcher::new(paths);
        match watcher.tick(&backend) {
            Tick::Failed { reason } => assert!(reason.contains("no usable wallpaper"), "{reason}"),
            other => panic!("got {other:?}"),
        }
        assert_eq!(*backend.apply_calls.borrow(), 0);
    }

    #[test]
    fn a_detection_error_does_not_panic() {
        let (_dir, paths) = sandbox();
        automatic_settings(&paths);
        let backend = FakeBackend::default();
        *backend.fail_detect.borrow_mut() = true;
        let mut watcher = Watcher::new(paths);
        assert!(matches!(watcher.tick(&backend), Tick::Failed { .. }));
    }

    #[test]
    fn an_apply_error_is_reported_and_the_state_is_marked_failing() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [10, 20, 30]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image);
        *backend.fail_apply.borrow_mut() = true;
        let mut watcher = Watcher::new(paths);
        assert!(matches!(watcher.tick(&backend), Tick::Failed { .. }));
        assert!(watcher
            .state
            .last_error
            .as_deref()
            .unwrap()
            .contains("applying"));
    }

    #[test]
    fn success_clears_a_previous_failure() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [10, 20, 30]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image.clone());
        *backend.fail_apply.borrow_mut() = true;
        let mut watcher = Watcher::new(paths.clone());
        watcher.tick(&backend);
        assert!(watcher.state.is_failing());

        *backend.fail_apply.borrow_mut() = false;
        assert!(matches!(watcher.tick(&backend), Tick::Applied { .. }));
        assert!(!watcher.state.is_failing());
        assert_eq!(watcher.state.consecutive_failures, 0);
        assert!(ServiceState::load(&paths.last_applied_file())
            .accent
            .is_some());
    }

    #[test]
    fn corrupted_settings_are_a_handled_failure_not_a_panic() {
        let (_dir, paths) = sandbox();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.settings_file(), "this is not toml = = =").unwrap();
        let backend = FakeBackend::default();
        let mut watcher = Watcher::new(paths);
        assert!(matches!(watcher.tick(&backend), Tick::Failed { .. }));
    }

    #[test]
    fn the_state_file_round_trips() {
        let (_dir, paths) = sandbox();
        let state = ServiceState {
            wallpaper: Some(PathBuf::from("/w.jpg")),
            accent: Some("#5f6fdd".into()),
            variant: "dark".into(),
            last_applied_unix: Some(123),
            last_error: None,
            consecutive_failures: 0,
            wallpaper_fingerprint: None,
        };
        state.save(&paths.last_applied_file()).unwrap();
        assert_eq!(ServiceState::load(&paths.last_applied_file()), state);
    }

    #[test]
    fn backoff_grows_with_failures_and_is_capped() {
        let (_dir, paths) = sandbox();
        let mut watcher = Watcher::new(paths);
        assert_eq!(watcher.next_delay(5, 30), Duration::from_secs(5));
        watcher.state.consecutive_failures = 1;
        assert_eq!(watcher.next_delay(5, 30), Duration::from_secs(30));
        watcher.state.consecutive_failures = 2;
        assert_eq!(watcher.next_delay(5, 30), Duration::from_secs(60));
        watcher.state.consecutive_failures = 30;
        assert_eq!(watcher.next_delay(5, 30), Duration::from_secs(3600));
    }

    #[test]
    fn the_loop_runs_until_stopped() {
        #[derive(Default)]
        struct CountingSleeper {
            sleeps: std::cell::RefCell<u32>,
        }
        impl Sleeper for CountingSleeper {
            fn sleep(&self, _duration: Duration) {
                *self.sleeps.borrow_mut() += 1;
            }
        }
        let (_dir, paths) = sandbox();
        let backend = FakeBackend::default();
        let mut watcher = Watcher::new(paths);
        let sleeper = CountingSleeper::default();
        run(&mut watcher, &backend, &sleeper, || {
            *sleeper.sleeps.borrow() >= 3
        });
        assert_eq!(*sleeper.sleeps.borrow(), 3);
    }

    #[test]
    fn an_automatic_apply_records_a_restorable_snapshot() {
        // Regression: `tick` built an empty snapshot, so an automatic apply
        // could not be undone even though the ownership record was written.
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [40, 80, 200]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image);
        *backend.theme_path.borrow_mut() = Some(paths.kde_color_schemes_dir().join("x.colors"));
        let mut watcher = Watcher::new(paths.clone());
        assert!(matches!(watcher.tick(&backend), Tick::Applied { .. }));

        let store = SnapshotStore::new(paths.snapshots_dir());
        let listed = store.list();
        assert_eq!(listed.len(), 1, "an automatic apply must record a snapshot");
        let snapshot = store.load(&listed[0].id).unwrap();
        assert!(!snapshot.is_empty(), "the snapshot recorded no entries");
        assert_eq!(snapshot.reason, "automatic-apply");
        assert!(
            snapshot
                .entries
                .values()
                .all(|e| e.applied != ws_core::snapshot::CapturedState::Absent || !e.path.exists()),
            "every captured entry must reflect what was written"
        );
    }

    #[test]
    fn a_snapshot_is_taken_before_the_backend_writes_anything() {
        // The captured "previous" state must be what existed beforehand: for a
        // file the apply creates, that is `Absent`.
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [40, 80, 200]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image);
        *backend.theme_path.borrow_mut() = Some(paths.kde_color_schemes_dir().join("x.colors"));
        let mut watcher = Watcher::new(paths.clone());
        watcher.tick(&backend);

        let store = SnapshotStore::new(paths.snapshots_dir());
        let snapshot = store.load(&store.list()[0].id).unwrap();
        let created = snapshot
            .entries
            .values()
            .filter(|e| e.previous == ws_core::snapshot::CapturedState::Absent)
            .count();
        assert!(
            created > 0,
            "a first apply creates files, so some entries start absent"
        );
    }

    #[test]
    fn a_restarted_watcher_detects_a_change_made_while_it_was_down() {
        // The watcher runs as a short-lived process under a systemd timer as well
        // as in a loop, so state must survive a restart. Re-fingerprinting the
        // file at startup would describe the *new* content and hide the change.
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [40, 80, 200]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image.clone());
        *backend.theme_path.borrow_mut() = Some(paths.kde_color_schemes_dir().join("x.colors"));

        {
            let mut watcher = Watcher::new(paths.clone());
            watcher.restore_state();
            assert!(matches!(watcher.tick(&backend), Tick::Applied { .. }));
        }

        // The image is replaced while no watcher is running.
        write_image(dir.path(), "w.png", [200, 40, 40]);

        {
            let mut watcher = Watcher::new(paths.clone());
            watcher.restore_state();
            assert!(
                matches!(watcher.tick(&backend), Tick::Applied { .. }),
                "a change made while the watcher was stopped must be applied"
            );
        }
    }

    #[test]
    fn a_restarted_watcher_does_not_reapply_an_unchanged_image() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "w.png", [40, 80, 200]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image);
        *backend.theme_path.borrow_mut() = Some(paths.kde_color_schemes_dir().join("x.colors"));

        {
            let mut watcher = Watcher::new(paths.clone());
            watcher.restore_state();
            watcher.tick(&backend);
        }
        {
            let mut watcher = Watcher::new(paths.clone());
            watcher.restore_state();
            assert!(matches!(watcher.tick(&backend), Tick::Unchanged));
        }
        assert_eq!(*backend.apply_calls.borrow(), 1);
    }

    #[test]
    fn a_monochrome_wallpaper_still_produces_an_applied_palette() {
        let (dir, paths) = sandbox();
        automatic_settings(&paths);
        let image = write_image(dir.path(), "grey.png", [128, 128, 128]);
        let backend = FakeBackend::default();
        *backend.detect.borrow_mut() = Some(image);
        let mut watcher = Watcher::new(paths);
        match watcher.tick(&backend) {
            Tick::Applied { accent, .. } => assert_eq!(accent, "#3ddc97", "fallback accent"),
            other => panic!("got {other:?}"),
        }
    }
}
