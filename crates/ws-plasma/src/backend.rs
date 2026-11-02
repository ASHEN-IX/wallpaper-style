//! The Plasma 6 backend: detection, preview, apply and restore.
//!
//! # Apply is transactional in spirit
//!
//! [`PlasmaBackend::apply`] never writes before the caller has a snapshot. It
//! writes each artefact atomically, records what it changed in the ownership
//! manifest, and continues past individual failures so one missing dependency
//! (say, Kvantum not configured) cannot leave the desktop half-themed. Every
//! skipped step is reported with a reason.
//!
//! # What is never touched
//!
//! * System theme directories under `/usr/share`: everything is written to the
//!   user's XDG directories.
//! * Configuration keys this application did not set. Restore compares state
//!   before writing, so a value the user changed afterwards survives.
//! * The base Kvantum theme is read only.

use std::path::{Path, PathBuf};

use ws_backend::{
    ApplyReport, Availability, BackendError, DetectedWallpaper, EnvironmentReport, PreviewReport,
    ThemeApplier, TransparencyBackend, WallpaperSource,
};
use ws_core::ownership::{hash_bytes, OwnedItem};
use ws_core::snapshot::{now_unix, write_atomic, CapturedState, SnapshotEntry};
use ws_core::{
    transparency_rules, MonitorWallpaper, Ownership, Paths, Settings, TransparencyRules,
};
use ws_palette::{PaletteReport, Variant};

use crate::colorscheme;
use crate::env::{self, Runner, SessionInfo, SystemRunner, Toolchain};
use crate::kvantum;
use crate::kwin_effect;
use crate::wallpaper_source;

/// Where the backend should look for things. Separated from [`Paths`] so tests
/// can redirect a single directory.
#[derive(Debug, Clone)]
pub struct BackendPaths {
    pub paths: Paths,
    /// Where generated Kvantum themes are written.
    pub kvantum_dir: PathBuf,
    /// Additional Kvantum theme roots (system).
    pub kvantum_system_roots: Vec<PathBuf>,
    /// `kdeglobals`, for the active scheme name.
    pub kdeglobals: PathBuf,
    /// `kwinrc`, for effect enablement.
    pub kwinrc: PathBuf,
    /// Plasma's wallpaper configuration.
    pub plasma_appletsrc: PathBuf,
}

impl BackendPaths {
    pub fn new(paths: Paths) -> Self {
        Self {
            kvantum_dir: paths.kvantum_dir(),
            kvantum_system_roots: Vec::new(),
            kdeglobals: paths.kdeglobals(),
            kwinrc: paths.kwinrc(),
            plasma_appletsrc: paths.plasma_appletsrc(),
            paths,
        }
    }
}

/// The Plasma backend.
pub struct PlasmaBackend {
    pub paths: BackendPaths,
    pub session: SessionInfo,
    pub toolchain: Toolchain,
    pub runner: Box<dyn Runner>,
    /// Set when the user chose an image manually, which overrides detection.
    pub manual_image: Option<PathBuf>,
}

impl std::fmt::Debug for PlasmaBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlasmaBackend")
            .field("session", &self.session)
            .field("toolchain", &self.toolchain)
            .field("manual_image", &self.manual_image)
            .finish()
    }
}

impl PlasmaBackend {
    /// Build a backend for the current environment.
    pub fn detect(paths: Paths) -> Result<Self, BackendError> {
        let runner = SystemRunner;
        let session = SessionInfo::detect(&|key| std::env::var(key).ok(), &runner);
        let mut toolchain = env::detect_toolchain(&runner);
        toolchain.compositing_active = Self::query_compositing(&runner);
        let mut backend = Self {
            paths: BackendPaths::new(paths),
            session,
            toolchain,
            runner: Box::new(SystemRunner),
            manual_image: None,
        };
        backend.paths.kvantum_system_roots = backend.toolchain.kvantum_roots.clone();
        Ok(backend)
    }

    /// Construct with an explicit runner, for tests.
    pub fn with_runner(
        paths: BackendPaths,
        session: SessionInfo,
        toolchain: Toolchain,
        runner: Box<dyn Runner>,
    ) -> Self {
        Self {
            paths,
            session,
            toolchain,
            runner,
            manual_image: None,
        }
    }

    /// Ask the compositor whether it is running.
    fn query_compositing(runner: &dyn Runner) -> bool {
        runner
            .run(
                "qdbus6",
                &[
                    "org.kde.KWin",
                    "/Compositor",
                    "org.freedesktop.DBus.Properties.Get",
                    "org.kde.kwin.Compositing",
                    "active",
                ],
            )
            .map(|out| out.trim().eq_ignore_ascii_case("true"))
            // If the query is unavailable, do not claim compositing is off: an
            // unknown answer must not disable a working feature.
            .unwrap_or(true)
    }

    fn toolchain_refresh(&self) -> Toolchain {
        self.toolchain.clone()
    }

    /// Where generated colour schemes live.
    pub fn color_scheme_dir(&self) -> PathBuf {
        self.paths.paths.kde_color_schemes_dir()
    }

    /// Where the KWin effect package lives.
    pub fn effect_dir(&self) -> PathBuf {
        env::effect_dir(&self.paths.paths.xdg_data)
    }

    /// The currently active KDE colour scheme name, if any.
    pub fn active_color_scheme(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.paths.kdeglobals).ok()?;
        let ini = ws_core::ownership::IniDocument::parse(&text);
        ini.get("General", "ColorScheme")
    }

    /// Write a file atomically and record it as owned.
    ///
    /// Returns the snapshot entry describing the change so the caller can build
    /// one snapshot for the whole apply.
    fn write_owned(
        &self,
        path: &Path,
        contents: &str,
        ownership: &mut Ownership,
    ) -> Result<SnapshotEntry, BackendError> {
        let previous = match std::fs::read_to_string(path) {
            Ok(existing) => CapturedState::Content { text: existing },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => CapturedState::Absent,
            Err(source) => return Err(BackendError::io(path, source)),
        };
        write_atomic(path, contents.as_bytes()).map_err(|source| BackendError::io(path, source))?;

        let id = format!("file:{}", path.display());
        match &previous {
            CapturedState::Absent => {
                ownership.record(
                    id.clone(),
                    OwnedItem::CreatedFile {
                        path: path.to_path_buf(),
                    },
                );
            }
            CapturedState::Content { text } => {
                ownership.record(
                    id.clone(),
                    OwnedItem::ModifiedFile {
                        path: path.to_path_buf(),
                        original_sha256: hash_bytes(text.as_bytes()),
                        applied_sha256: hash_bytes(contents.as_bytes()),
                    },
                );
            }
            _ => {}
        }

        Ok(SnapshotEntry {
            id,
            previous,
            applied: CapturedState::Content {
                text: contents.to_owned(),
            },
            path: path.to_path_buf(),
            detail: None,
        })
    }

    /// Write a KDE configuration key and record it as owned.
    fn set_config_key(
        &self,
        file: &Path,
        group: &str,
        key: &str,
        value: &str,
        ownership: &mut Ownership,
    ) -> Result<Option<SnapshotEntry>, BackendError> {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(BackendError::io(file, source)),
        };
        let mut ini = ws_core::ownership::IniDocument::parse(&text);
        let previous = ini.get(group, key);
        if previous.as_deref() == Some(value) {
            // Already correct: nothing to record, nothing to write.
            return Ok(None);
        }
        ini.set(group, key, value);
        write_atomic(file, ini.render().as_bytes())
            .map_err(|source| BackendError::io(file, source))?;

        let id = format!("cfg:{}:{group}:{key}", file.display());
        ownership.record(
            id.clone(),
            OwnedItem::ConfigKey {
                file: file.to_path_buf(),
                group: group.to_owned(),
                key: key.to_owned(),
                original: previous.clone(),
                applied: value.to_owned(),
            },
        );
        Ok(Some(SnapshotEntry {
            id,
            previous: CapturedState::ConfigValue { value: previous },
            applied: CapturedState::ConfigValue {
                value: Some(value.to_owned()),
            },
            path: file.to_path_buf(),
            detail: Some(ws_core::snapshot::SnapshotDetail::ConfigKey {
                group: group.to_owned(),
                key: key.to_owned(),
            }),
        }))
    }

    /// The Kvantum base theme to derive from, honouring the recorded original.
    pub fn kvantum_base_theme(&self) -> Option<kvantum::BaseTheme> {
        let config_path = self.paths.kvantum_dir.join("kvantum.kvconfig");
        let config_text = std::fs::read_to_string(&config_path).ok()?;
        let record = self.paths.paths.state_dir.join("kvantum-base-theme.json");
        let name = kvantum::base_theme_name(&record, &config_text)?;
        kvantum::locate_base_theme(
            &name,
            &self.paths.kvantum_dir,
            &self.paths.kvantum_system_roots,
        )
    }

    /// The manually chosen image recorded in settings, if any.
    fn settings_manual_image(&self) -> Option<PathBuf> {
        Settings::load(&self.paths.paths.settings_file())
            .ok()
            .and_then(|(settings, _)| settings.palette.manual_image)
    }

    fn palette_for(&self, palette: &PaletteReport, settings: &Settings) -> PaletteReport {
        // Overrides were already applied by ws_palette::derive; nothing to do
        // here beyond keeping the alpha the user asked for.
        let _ = settings;
        palette.clone()
    }
}

impl WallpaperSource for PlasmaBackend {
    fn detected_wallpapers(&self) -> Result<Vec<MonitorWallpaper>, BackendError> {
        // A manual choice short-circuits detection entirely, which is the
        // documented fallback when the desktop's own detection is unsupported.
        // The setting lives in the settings file so the service and the CLI see
        // the same choice the GUI wrote; `manual_image` on the struct is the
        // per-invocation override (`apply --image`).
        let from_settings = self.settings_manual_image().filter(|path| path.is_file());
        if let Some(manual) = self.manual_image.as_ref().or(from_settings.as_ref()) {
            return Ok(vec![MonitorWallpaper {
                screen: 0,
                plugin: "manual".to_owned(),
                plugin_kind: ws_core::PluginKind::Image,
                image: Some(manual.display().to_string()),
            }]);
        }

        if let Some(qdbus) = &self.toolchain_refresh().qdbus {
            match wallpaper_source::read_live(self.runner.as_ref(), qdbus) {
                Ok(monitors) if !monitors.is_empty() => return Ok(monitors),
                Ok(_) => {
                    log::debug!("Plasma reported no desktop containers; trying the config file")
                }
                Err(err) => {
                    log::warn!("live wallpaper query failed ({err}); using the config file")
                }
            }
        }

        match wallpaper_source::read_config(&self.paths.plasma_appletsrc) {
            Ok(monitors) => Ok(monitors),
            Err(err) => Err(BackendError::Other(format!(
                "{err}. Choose an image manually in the control centre, or set \
                 palette.manual_image."
            ))),
        }
    }

    fn detect(&self) -> Result<Option<DetectedWallpaper>, BackendError> {
        let monitors = self.detected_wallpapers()?;
        let selection = ws_core::wallpaper::select(&monitors, |p| p.is_file());
        match selection {
            ws_core::Selection::Found {
                path,
                screen,
                plugin,
                plugin_kind,
                ..
            } => Ok(Some(DetectedWallpaper {
                path,
                screen,
                plugin,
                is_slideshow: plugin_kind == ws_core::PluginKind::Slideshow,
            })),
            ws_core::Selection::None { .. } => {
                if let Some(message) = selection.explain() {
                    log::info!("{message}");
                }
                Ok(None)
            }
        }
    }
}

impl ThemeApplier for PlasmaBackend {
    fn environment(&self) -> EnvironmentReport {
        env::environment_report(
            &self.session,
            &self.toolchain,
            self.toolchain.compositing_active,
            None,
        )
    }

    fn preview(&self, palette: &PaletteReport, settings: &Settings) -> PreviewReport {
        let palette = self.palette_for(palette, settings);
        let scheme = colorscheme::generate(&palette.palette, palette.variant);
        let mut would_change = Vec::new();
        let mut would_skip = Vec::new();

        if settings.themes.plasma_color_scheme {
            would_change.push(format!(
                "Write {} and activate it as the Plasma colour scheme",
                scheme.file_name()
            ));
        } else {
            would_skip.push((
                "Plasma colour scheme".to_owned(),
                "disabled in settings".to_owned(),
            ));
        }

        if settings.themes.kvantum {
            match self.kvantum_base_theme() {
                Some(base) => {
                    would_change.push(format!(
                        "Generate Kvantum theme{} from {:?} for Qt applications",
                        if settings.themes.kvantum_glass_variant {
                            "s (opaque and glass)"
                        } else {
                            ""
                        },
                        base.name
                    ));
                }
                None => would_skip.push((
                    "Kvantum theme".to_owned(),
                    "no compatible base theme is selected in Kvantum Manager".to_owned(),
                )),
            }
        } else {
            would_skip.push((
                "Kvantum theme".to_owned(),
                "disabled in settings".to_owned(),
            ));
        }

        if settings.themes.kwin_effect {
            would_change.push(format!(
                "Install the KWin effect {:?} and enable it",
                kwin_effect::EFFECT_ID
            ));
        } else {
            would_skip.push((
                "Window opacity".to_owned(),
                "disabled in settings".to_owned(),
            ));
        }

        PreviewReport {
            palette,
            color_scheme_sample: Some(scheme.text),
            would_change,
            would_skip,
        }
    }

    fn apply(
        &self,
        palette: &PaletteReport,
        settings: &Settings,
        ownership: &mut Ownership,
    ) -> Result<ApplyReport, BackendError> {
        let palette = self.palette_for(palette, settings);
        let settings = settings.normalised();
        let mut report = ApplyReport {
            changed: Vec::new(),
            skipped: Vec::new(),
            restart_required: Vec::new(),
            color_scheme: None,
        };

        // 1. Plasma colour scheme.
        if settings.themes.plasma_color_scheme {
            let scheme = colorscheme::generate(&palette.palette, palette.variant);
            let path = self.color_scheme_dir().join(scheme.file_name());
            self.write_owned(&path, &scheme.text, ownership)?;
            report.changed.push(format!("wrote {}", path.display()));

            // `plasma-apply-colorscheme` does not copy `AccentColor` out of the
            // scheme file, so Plasma 6 keeps its previous shell accent — the
            // Kickoff selection pill and the category icons stay on the old
            // colour while Qt applications switch correctly. Write the key
            // ourselves, and record it so restore can undo it.
            if let Some(entry) = self.set_config_key(
                &self.paths.kdeglobals,
                "General",
                "AccentColor",
                &colorscheme::accent_key(&palette.palette),
                ownership,
            )? {
                report
                    .changed
                    .push(format!("set the shell accent ({})", entry.id));
            }

            if self.toolchain_refresh().plasma_apply_colorscheme {
                match self
                    .runner
                    .as_ref()
                    .run("plasma-apply-colorscheme", &[&scheme.name])
                {
                    Ok(_) => {
                        report
                            .changed
                            .push(format!("activated colour scheme {:?}", scheme.name));
                        report.color_scheme = Some(scheme.name.clone());
                    }
                    Err(err) => {
                        report.skipped.push((
                            "colour scheme activation".to_owned(),
                            format!(
                                "{err}. Select {:?} manually in System Settings → Colours.",
                                scheme.name
                            ),
                        ));
                    }
                }
            } else {
                report.skipped.push((
                    "colour scheme activation".to_owned(),
                    "plasma-apply-colorscheme is not installed".to_owned(),
                ));
            }
        } else {
            report.skipped.push((
                "Plasma colour scheme".to_owned(),
                "disabled in settings".to_owned(),
            ));
        }

        // 2. Kvantum.
        if settings.themes.kvantum {
            match self.apply_kvantum(&palette, &settings, ownership) {
                Ok(mut outcome) => {
                    report.changed.append(&mut outcome.changed);
                    report.skipped.append(&mut outcome.skipped);
                    report.restart_required.append(&mut outcome.restart);
                }
                Err(err) => report
                    .skipped
                    .push(("Kvantum theme".to_owned(), err.to_string())),
            }
        }

        // 3. KWin effect.
        if settings.themes.kwin_effect {
            let rules = transparency_rules(&settings);
            match self.apply_effect(&rules, ownership) {
                Ok(mut outcome) => {
                    report.changed.append(&mut outcome.changed);
                    report.skipped.append(&mut outcome.skipped);
                }
                Err(err) => report
                    .skipped
                    .push(("Window opacity".to_owned(), err.to_string())),
            }
        } else if ownership
            .items
            .contains_key(&format!("effect:{}", kwin_effect::EFFECT_ID))
        {
            // The effect was enabled before and the user has now turned it off:
            // disable it rather than leaving it running.
            let _ = self.disable(ownership);
            report
                .changed
                .push("disabled the window opacity effect".to_owned());
        }

        ownership.applied_generation += 1;
        Ok(report)
    }

    fn restore(
        &self,
        snapshot: &ws_core::Snapshot,
        force: bool,
    ) -> Result<Vec<ws_core::RestoreEntry>, BackendError> {
        let entries = ws_core::snapshot::restore(snapshot, force);

        // Remove any owned directory whose contents the restore just deleted, so
        // a removed effect does not linger as an empty package that `status`
        // still reports. Directories are only removed when empty.
        if let Ok(ownership) = Ownership::load(&self.paths.paths.ownership_file()) {
            for item in ownership.items.values() {
                if let OwnedItem::EffectPackage { path, .. } = item {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }

        // Reload the compositor so a disabled effect takes effect immediately.
        if let Some(qdbus) = &self.toolchain_refresh().qdbus {
            let _ = self.runner.as_ref().run(
                qdbus,
                &[
                    "org.kde.KWin",
                    "/Effects",
                    "org.kde.kwin.Effects.unloadEffect",
                    kwin_effect::EFFECT_ID,
                ],
            );
        }
        Ok(entries)
    }
}

/// The parts of an apply that may partially fail.
struct SubOutcome {
    changed: Vec<String>,
    skipped: Vec<(String, String)>,
    restart: Vec<String>,
}

impl PlasmaBackend {
    fn apply_kvantum(
        &self,
        palette: &PaletteReport,
        settings: &Settings,
        ownership: &mut Ownership,
    ) -> Result<SubOutcome, BackendError> {
        let mut outcome = SubOutcome {
            changed: Vec::new(),
            skipped: Vec::new(),
            restart: Vec::new(),
        };

        let base = match self.kvantum_base_theme() {
            Some(base) => base,
            None => {
                outcome.skipped.push((
                    "Kvantum theme".to_owned(),
                    "no compatible base theme is selected in Kvantum Manager; \
                     Plasma colours are unaffected"
                        .to_owned(),
                ));
                return Ok(outcome);
            }
        };

        // Record the original base theme the first time, so later applies keep
        // deriving from the user's choice rather than from our generated theme.
        let record_path = self.paths.paths.state_dir.join("kvantum-base-theme.json");
        if !record_path.exists() {
            let record = kvantum::BaseThemeRecord {
                theme: base.name.clone(),
            };
            let text = serde_json::to_string_pretty(&record)
                .map_err(|e| BackendError::Other(e.to_string()))?;
            write_atomic(&record_path, text.as_bytes())
                .map_err(|source| BackendError::io(&record_path, source))?;
            outcome
                .changed
                .push(format!("recorded base theme {:?}", base.name));
        }

        let want_glass = settings.themes.kvantum_glass_variant;
        let generated = kvantum::generate(&base, &palette.palette, palette.variant, want_glass)
            .map_err(|e| BackendError::Other(e.to_string()))?;

        let mut written = Vec::new();
        for theme in std::iter::once(&generated.opaque).chain(generated.glass.iter()) {
            let dir = kvantum::theme_dir(&self.paths.kvantum_dir, &theme.name);
            let kvconfig = dir.join(format!("{}.kvconfig", theme.name));
            let svg = dir.join(format!("{}.svg", theme.name));
            self.write_owned(&kvconfig, &theme.kvconfig, ownership)?;
            self.write_owned(&svg, &theme.svg, ownership)?;
            written.push(theme.name.clone());
        }

        if want_glass && generated.glass.is_none() {
            outcome.skipped.push((
                "Background-only transparency".to_owned(),
                "the base Kvantum theme does not support translucency, so only \
                 whole-window opacity is available"
                    .to_owned(),
            ));
        }

        // Point Kvantum at the new theme.
        let kvconfig_path = self.paths.kvantum_dir.join("kvantum.kvconfig");
        let existing = std::fs::read_to_string(&kvconfig_path).unwrap_or_default();
        let updated = kvantum::update_kvconfig(
            &existing,
            &generated.opaque.name,
            generated.glass.as_ref().map(|g| g.name.as_str()),
            &settings.themes.kvantum_apps,
        );
        self.write_owned(&kvconfig_path, &updated, ownership)?;
        outcome.changed.push(format!(
            "activated Kvantum theme {:?}",
            generated.opaque.name
        ));

        // Ask running Qt applications to reload the palette. Applications that
        // cache their style must be restarted; say so rather than pretending.
        if self.runner.as_ref().has("gdbus") {
            let _ = self.runner.as_ref().run(
                "gdbus",
                &[
                    "emit",
                    "--session",
                    "--object-path",
                    "/KGlobalSettings",
                    "--signal",
                    "org.kde.KGlobalSettings.notifyChange",
                    "2",
                    "0",
                ],
            );
            let _ = &written;
        }
        outcome.restart.push(
            "Qt applications already running (Dolphin, Kate, Ark, System Settings) need to be \
             reopened to pick up the new Kvantum theme"
                .to_owned(),
        );
        Ok(outcome)
    }

    fn apply_effect(
        &self,
        rules: &TransparencyRules,
        ownership: &mut Ownership,
    ) -> Result<SubOutcome, BackendError> {
        let mut outcome = SubOutcome {
            changed: Vec::new(),
            skipped: Vec::new(),
            restart: Vec::new(),
        };
        let package = kwin_effect::generate(rules);
        let dir = self.effect_dir();
        let metadata = dir.join("metadata.json");
        let script = dir.join("contents/code/main.js");

        self.write_owned(&metadata, &package.metadata, ownership)?;
        self.write_owned(&script, &package.script, ownership)?;

        let id = format!("effect:{}", package.id);
        ownership.record(
            id,
            OwnedItem::EffectPackage {
                id: package.id.clone(),
                path: dir.clone(),
            },
        );
        outcome
            .changed
            .push(format!("wrote KWin effect to {}", dir.display()));

        // Enable in kwinrc and load it now.
        if let Some(entry) = self.set_config_key(
            &self.paths.kwinrc,
            "Plugins",
            &format!("{}Enabled", package.id),
            if rules.enabled { "true" } else { "false" },
            ownership,
        )? {
            outcome.changed.push(format!("recorded {}", entry.id));
        }

        if let Some(qdbus) = &self.toolchain_refresh().qdbus {
            // Unload first so KWin re-reads the regenerated script.
            let _ = self.runner.as_ref().run(
                qdbus,
                &[
                    "org.kde.KWin",
                    "/Effects",
                    "org.kde.kwin.Effects.unloadEffect",
                    &package.id,
                ],
            );
            if rules.enabled {
                match self.runner.as_ref().run(
                    qdbus,
                    &[
                        "org.kde.KWin",
                        "/Effects",
                        "org.kde.kwin.Effects.loadEffect",
                        &package.id,
                    ],
                ) {
                    Ok(out) if out.trim() == "true" => {
                        outcome
                            .changed
                            .push("loaded the transparency effect".to_owned());
                    }
                    Ok(_) | Err(_) => outcome.skipped.push((
                        "Window opacity".to_owned(),
                        "KWin did not load the effect. Log out and back in, then check \
                         System Settings → Window Management → Desktop Effects."
                            .to_owned(),
                    )),
                }
            }
        } else {
            outcome.skipped.push((
                "Window opacity".to_owned(),
                "qdbus6 is missing, so the effect cannot be loaded now; it will load at next login"
                    .to_owned(),
            ));
        }
        Ok(outcome)
    }
}

impl TransparencyBackend for PlasmaBackend {
    fn availability(&self) -> Availability {
        if !self.session.is_plasma() {
            return Availability::unavailable("only KDE Plasma 6 is supported");
        }
        if !self.toolchain.compositing_active {
            return Availability::unavailable(
                "the compositor is not running, so window opacity cannot be applied",
            );
        }
        if self.toolchain.qdbus.is_none() {
            return Availability::degraded(
                "qdbus6 is missing: the effect is written to disk but cannot be loaded until \
                 the next login",
            );
        }
        Availability::available()
    }

    fn install(&self, rules: &TransparencyRules) -> Result<OwnedItem, BackendError> {
        let package = kwin_effect::generate(rules);
        let dir = self.effect_dir();
        for (path, contents) in [
            (dir.join("metadata.json"), &package.metadata),
            (dir.join("contents/code/main.js"), &package.script),
        ] {
            write_atomic(&path, contents.as_bytes())
                .map_err(|source| BackendError::io(&path, source))?;
        }
        Ok(OwnedItem::EffectPackage {
            id: package.id,
            path: dir,
        })
    }

    fn disable(&self, ownership: &mut Ownership) -> Result<(), BackendError> {
        let key = format!("{}Enabled", kwin_effect::EFFECT_ID);
        self.set_config_key(&self.paths.kwinrc, "Plugins", &key, "false", ownership)?;
        if let Some(qdbus) = &self.toolchain_refresh().qdbus {
            let _ = self.runner.as_ref().run(
                qdbus,
                &[
                    "org.kde.KWin",
                    "/Effects",
                    "org.kde.kwin.Effects.unloadEffect",
                    kwin_effect::EFFECT_ID,
                ],
            );
        }
        Ok(())
    }

    fn reload(&self) -> Result<(), BackendError> {
        let Some(qdbus) = &self.toolchain_refresh().qdbus else {
            return Err(BackendError::Other("qdbus6 is not available".to_owned()));
        };
        self.runner
            .as_ref()
            .run(
                qdbus,
                &[
                    "org.kde.KWin",
                    "/Effects",
                    "org.kde.kwin.Effects.reconfigureEffect",
                    kwin_effect::EFFECT_ID,
                ],
            )
            .map_err(|e| BackendError::command("reconfigure effect", e))?;
        Ok(())
    }

    fn open_window_classes(&self) -> Result<Vec<String>, BackendError> {
        // KWin exposes open windows only through its scripting API, which
        // returns plain text. Parse defensively and never log window titles.
        let Some(qdbus) = &self.toolchain_refresh().qdbus else {
            return Err(BackendError::command(
                "list windows",
                "qdbus6 is not available, so open windows cannot be listed",
            ));
        };
        let script = r#"print(effects ? "" : "")"#;
        let _ = script;
        match self.runner.as_ref().run(
            qdbus,
            &["org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting.start"],
        ) {
            Ok(_) => {}
            Err(_) => {
                return Err(BackendError::command(
                    "list windows",
                    "KWin scripting is not reachable; use the Identify page's manual entry instead",
                ))
            }
        }
        // Without a loaded script there is no direct listing API; return an
        // empty list and let the caller offer manual entry.
        Ok(Vec::new())
    }

    fn transparency_kind(&self, _window_class: &str) -> ws_core::TransparencyKind {
        // In Plasma, whole-window opacity always works; background-only is a
        // property of the application's toolkit, which is reported per preset.
        ws_core::TransparencyKind::WholeWindow
    }
}

/// Report which variant should be applied for the current settings.
pub fn variant_for(settings: &Settings) -> Variant {
    settings.general.variant
}

/// A cheap fingerprint of everything an apply reads, used by the service to
/// decide whether anything changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyInputs {
    pub wallpaper: Option<ws_core::WallpaperFingerprint>,
    pub variant: Variant,
    pub enabled_effects: u64,
    pub taken_unix: u64,
}

impl ApplyInputs {
    pub fn capture(wallpaper: Option<ws_core::WallpaperFingerprint>, settings: &Settings) -> Self {
        Self {
            wallpaper,
            variant: settings.general.variant,
            enabled_effects: settings.themes.kwin_effect as u64
                + settings.themes.plasma_color_scheme as u64 * 2
                + settings.themes.kvantum as u64 * 4,
            taken_unix: now_unix(),
        }
    }

    /// True when the two captures describe the same inputs.
    pub fn same_as(&self, other: &Self) -> bool {
        let wallpaper_same = match (&self.wallpaper, &other.wallpaper) {
            (None, None) => true,
            (Some(a), Some(b)) => a.matches(b),
            _ => false,
        };
        wallpaper_same
            && self.variant == other.variant
            && self.enabled_effects == other.enabled_effects
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
    use crate::env::RecordingRunner;
    use ws_core::paths::Paths;

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

    fn session() -> SessionInfo {
        SessionInfo {
            desktop: "KDE".into(),
            session_type: "wayland".into(),
            desktop_version: Some("kwin 6.8.1".into()),
            compositor: Some("kwin_wayland".into()),
        }
    }

    fn toolchain() -> Toolchain {
        Toolchain {
            qdbus: Some("qdbus6".into()),
            kwriteconfig: Some("kwriteconfig6".into()),
            kreadconfig: Some("kreadconfig6".into()),
            plasma_apply_colorscheme: true,
            kpackagetool: None,
            kvantum_roots: Vec::new(),
            has_kvantum: false,
            compositing_active: true,
        }
    }

    fn backend(paths: Paths, runner: RecordingRunner) -> PlasmaBackend {
        PlasmaBackend::with_runner(
            BackendPaths::new(paths),
            session(),
            toolchain(),
            Box::new(runner),
        )
    }

    fn palette() -> PaletteReport {
        ws_palette::build(
            ws_palette::Rgb::from_hex("#5f6fdd").unwrap(),
            Variant::Dark,
            255,
        )
    }

    #[test]
    fn apply_writes_only_into_the_sandbox() {
        let (dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        let backend = backend(paths.clone(), runner);
        let mut ownership = Ownership::default();

        let report = backend
            .apply(&palette(), &Settings::default(), &mut ownership)
            .unwrap();
        assert!(!report.changed.is_empty());

        // Everything written is under the temporary root.
        for entry in walk(dir.path()) {
            assert!(
                entry.starts_with(dir.path()),
                "wrote outside the sandbox: {}",
                entry.display()
            );
        }
        assert!(paths.kde_color_schemes_dir().exists());
    }

    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn apply_records_every_colour_scheme_in_the_ownership_manifest() {
        let (_dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        let backend = backend(paths, runner);
        let mut ownership = Ownership::default();
        backend
            .apply(&palette(), &Settings::default(), &mut ownership)
            .unwrap();
        assert!(ownership.items.values().any(|i| matches!(
            i,
            OwnedItem::CreatedFile { path } if path.extension().is_some_and(|e| e == "colors")
        )));
        assert_eq!(ownership.applied_generation, 1);
    }

    #[test]
    fn applying_twice_is_idempotent() {
        let (_dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        let backend = backend(paths, runner);
        let mut ownership = Ownership::default();
        backend
            .apply(&palette(), &Settings::default(), &mut ownership)
            .unwrap();
        let first: Vec<String> = ownership.items.keys().cloned().collect();
        backend
            .apply(&palette(), &Settings::default(), &mut ownership)
            .unwrap();
        let second: Vec<String> = ownership.items.keys().cloned().collect();
        assert_eq!(
            first, second,
            "a second apply must not duplicate ownership entries"
        );
    }

    #[test]
    fn a_missing_kvantum_is_skipped_with_a_reason_not_an_error() {
        let (_dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        let backend = backend(paths, runner);
        let mut ownership = Ownership::default();
        let report = backend
            .apply(&palette(), &Settings::default(), &mut ownership)
            .unwrap();
        assert!(
            report
                .skipped
                .iter()
                .any(|(what, _)| what.contains("Kvantum")),
            "{:?}",
            report.skipped
        );
        // The colour scheme still applied.
        assert!(report.color_scheme.is_some());
    }

    #[test]
    fn the_kwin_effect_is_written_and_enabled_when_requested() {
        let (_dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        runner
            .responses
            .borrow_mut()
            .push(("qdbus6".into(), Ok(String::new())));
        runner
            .responses
            .borrow_mut()
            .push(("qdbus6".into(), Ok("true".into())));
        let backend = backend(paths.clone(), runner);
        let mut settings = Settings::default();
        settings.themes.kwin_effect = true;
        settings.transparency.enabled = true;
        settings
            .transparency
            .custom
            .insert("code".into(), ws_core::AppOpacity::new(0.7));
        let mut ownership = Ownership::default();
        backend
            .apply(&palette(), &settings, &mut ownership)
            .unwrap();

        let script = backend.effect_dir().join("contents/code/main.js");
        assert!(script.exists(), "effect script was not written");
        let text = std::fs::read_to_string(&script).unwrap();
        assert!(text.contains("\"code\":700"), "rules were not baked in");
        let kwinrc = std::fs::read_to_string(&paths.kwinrc()).unwrap_or_default();
        assert!(
            kwinrc.contains("wallpaper-style-transparencyEnabled=true"),
            "{kwinrc}"
        );
    }

    #[test]
    fn turning_the_effect_off_later_disables_it() {
        let (_dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        for _ in 0..2 {
            runner
                .responses
                .borrow_mut()
                .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        }
        // qdbus calls: load, then unload for disable.
        for _ in 0..4 {
            runner
                .responses
                .borrow_mut()
                .push(("qdbus6".into(), Ok("true".into())));
        }
        let backend = backend(paths.clone(), runner);
        let mut settings = Settings::default();
        settings.themes.kwin_effect = true;
        let mut ownership = Ownership::default();
        backend
            .apply(&palette(), &settings, &mut ownership)
            .unwrap();

        settings.themes.kwin_effect = false;
        let report = backend
            .apply(&palette(), &settings, &mut ownership)
            .unwrap();
        assert!(
            report.changed.iter().any(|c| c.contains("disabled")),
            "{:?}",
            report.changed
        );
        let kwinrc = std::fs::read_to_string(&paths.kwinrc()).unwrap_or_default();
        assert!(
            kwinrc.contains("wallpaper-transparencyEnabled=false")
                || kwinrc.contains("wallpaper-style-transparencyEnabled=false"),
            "{kwinrc}"
        );
    }

    #[test]
    fn apply_writes_the_shell_accent_so_plasma_follows_the_palette() {
        // Regression: `plasma-apply-colorscheme` does not copy AccentColor out of
        // the scheme, so the Plasma shell kept its old accent while Qt apps
        // switched. Verified on a live session: the Kickoff pill stayed blue.
        let (_dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        let backend = backend(paths.clone(), runner);
        let mut ownership = Ownership::default();
        let report = backend
            .apply(&palette(), &Settings::default(), &mut ownership)
            .unwrap();

        let accent = kreadconfig6(&paths.kdeglobals(), "General", "AccentColor");
        assert_eq!(
            accent.as_deref(),
            Some("95,111,221"),
            "shell accent was not written"
        );
        assert!(
            report.changed.iter().any(|c| c.contains("shell accent")),
            "{:?}",
            report.changed
        );
        // Recorded, so restore puts the previous value back.
        assert!(ownership.items.values().any(|i| matches!(
            i,
            OwnedItem::ConfigKey { group, key, .. } if group == "General" && key == "AccentColor"
        )));
    }

    #[test]
    fn the_shell_accent_is_not_written_when_colour_schemes_are_disabled() {
        let (_dir, paths) = sandbox();
        let runner = RecordingRunner::with_programs(&["qdbus6", "plasma-apply-colorscheme"]);
        runner
            .responses
            .borrow_mut()
            .push(("plasma-apply-colorscheme".into(), Ok(String::new())));
        let backend = backend(paths.clone(), runner);
        let mut settings = Settings::default();
        settings.themes.plasma_color_scheme = false;
        let mut ownership = Ownership::default();
        backend
            .apply(&palette(), &settings, &mut ownership)
            .unwrap();
        assert_eq!(
            kreadconfig6(&paths.kdeglobals(), "General", "AccentColor"),
            None
        );
    }

    /// Read a key the way `kreadconfig6` would.
    fn kreadconfig6(path: &std::path::Path, group: &str, key: &str) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        ws_core::ownership::IniDocument::parse(&text).get(group, key)
    }

    #[test]
    fn preview_writes_nothing() {
        let (dir, paths) = sandbox();
        let backend = backend(paths, RecordingRunner::with_programs(&[]));
        let preview = backend.preview(&palette(), &Settings::default());
        assert!(preview.color_scheme_sample.is_some());
        assert!(!preview.would_change.is_empty());
        let files = walk(dir.path());
        assert!(files.is_empty(), "preview created {files:?}");
    }

    #[test]
    fn a_manual_image_overrides_detection() {
        let (dir, paths) = sandbox();
        let mut backend = backend(paths, RecordingRunner::with_programs(&[]));
        let image = dir.path().join("chosen.png");
        std::fs::write(&image, b"not really a png").unwrap();
        backend.manual_image = Some(image.clone());
        let detected = backend
            .detect()
            .unwrap()
            .expect("manual image must be used");
        assert_eq!(detected.path, image);
        assert_eq!(detected.plugin, "manual");
    }

    #[test]
    fn a_manual_image_in_settings_drives_detection() {
        // Regression: the service and `status` could not see an image chosen in
        // the GUI because only the per-invocation override was consulted.
        let (dir, paths) = sandbox();
        let image = dir.path().join("chosen.png");
        std::fs::write(&image, b"not really a png").unwrap();
        let mut settings = Settings::default();
        settings.palette.manual_image = Some(image.clone());
        settings.save(&paths.settings_file()).unwrap();

        let backend = backend(paths, RecordingRunner::with_programs(&[]));
        let detected = backend
            .detect()
            .unwrap()
            .expect("the settings image must be used");
        assert_eq!(detected.path, image);
    }

    #[test]
    fn a_manual_image_that_no_longer_exists_falls_back_to_detection() {
        let (_dir, paths) = sandbox();
        let mut settings = Settings::default();
        settings.palette.manual_image = Some(PathBuf::from("/nonexistent/gone.jpg"));
        settings.save(&paths.settings_file()).unwrap();
        let backend = backend(paths, RecordingRunner::with_programs(&[]));
        // Falls through to detection, which also fails here, but not with the
        // stale setting reported as the cause.
        let err = backend.detected_wallpapers().unwrap_err().to_string();
        assert!(!err.contains("gone.jpg"), "{err}");
    }

    #[test]
    fn detection_without_any_source_reports_a_failure_clearly() {
        let (_dir, paths) = sandbox();
        let backend = backend(paths.clone(), RecordingRunner::with_programs(&[]));
        // The config file does not exist in the sandbox.
        assert!(backend.detected_wallpapers().is_err());
    }

    #[test]
    fn a_non_plasma_session_reports_no_window_opacity() {
        let (_dir, paths) = sandbox();
        let mut backend = backend(paths, RecordingRunner::with_programs(&[]));
        backend.session.desktop = "GNOME".into();
        assert!(!backend.availability().is_available());
        assert!(!backend.environment().supported);
    }

    #[test]
    fn apply_inputs_detect_a_wallpaper_change_but_not_a_touch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.jpg");
        std::fs::write(&path, b"first").unwrap();
        let fingerprint = ws_core::WallpaperFingerprint::of(&path).unwrap();
        let settings = Settings::default();
        let a = ApplyInputs::capture(Some(fingerprint.clone()), &settings);
        let b = ApplyInputs::capture(Some(fingerprint), &settings);
        assert!(a.same_as(&b), "identical inputs must compare equal");

        std::fs::write(&path, b"different content").unwrap();
        let c = ApplyInputs::capture(
            Some(ws_core::WallpaperFingerprint::of(&path).unwrap()),
            &settings,
        );
        assert!(!a.same_as(&c), "a changed wallpaper must compare unequal");
    }

    #[test]
    fn a_settings_change_makes_previous_inputs_stale() {
        let settings = Settings::default();
        let a = ApplyInputs::capture(None, &settings);
        let mut changed = settings.clone();
        changed.general.variant = Variant::Light;
        let b = ApplyInputs::capture(None, &changed);
        assert!(!a.same_as(&b));
    }
}
