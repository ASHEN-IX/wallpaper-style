//! The control centre window.
//!
//! Structure follows the product's mental model rather than the code's:
//!
//! * **Overview** — what is happening now: wallpaper, palette, service health,
//!   and what is unavailable.
//! * **Palette** — inspect clusters, switch variant, override colours, and see
//!   contrast results.
//! * **Theme** — which integrations to write, with a preview and an explicit
//!   Apply.
//! * **Transparency** — per-app opacity, presets, exclusions.
//! * **Recovery** — snapshots, restore, uninstall.
//!
//! Long operations (sampling a 4K image, applying) run on a worker thread and
//! report through a channel, so the window never freezes.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;
use egui::{Color32, RichText};

use ws_backend::{Availability, Feature, Installer, ThemeApplier, WallpaperSource};
use ws_core::ownership::Ownership;
use ws_core::snapshot::SnapshotStore;
use ws_core::{presets, AppOpacity, Paths, Settings};
use ws_palette::{HuedCluster, PaletteOverrides, PaletteReport, Rgb, Variant};
use ws_plasma::backend::PlasmaBackend;
use ws_plasma::install::PlasmaInstaller;

use crate::theme::{self, Chrome, PillState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Palette,
    Theme,
    Transparency,
    Recovery,
}

impl Page {
    const ALL: [Page; 5] = [
        Page::Overview,
        Page::Palette,
        Page::Theme,
        Page::Transparency,
        Page::Recovery,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Palette => "Palette",
            Page::Theme => "Theme",
            Page::Transparency => "Transparency",
            Page::Recovery => "Recovery",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::Overview => "◉",
            Page::Palette => "◐",
            Page::Theme => "◧",
            Page::Transparency => "◫",
            Page::Recovery => "⟲",
        }
    }
}

/// Results from background work.
enum Worker {
    Scanned(ScanResult),
    Applied(Result<ApplySummary, String>),
    Restored(Result<String, String>),
    Installed(Result<String, String>),
}

struct ScanResult {
    wallpaper: Option<PathBuf>,
    clusters: Vec<HuedCluster>,
    accent: Rgb,
    accent_source: ws_palette::AccentSource,
    dark: PaletteReport,
    light: PaletteReport,
    monochrome: bool,
    slideshow: bool,
    error: Option<String>,
}

struct ApplySummary {
    changed: Vec<String>,
    skipped: Vec<(String, String)>,
    restart: Vec<String>,
    snapshot: Option<String>,
}

/// A toast message shown at the bottom of the window.
struct Toast {
    message: String,
    kind: ToastKind,
    born: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Info,
    Success,
    Error,
}

impl Toast {
    fn color(&self) -> Color32 {
        match self.kind {
            ToastKind::Info => Chrome::TEXT_DIM,
            ToastKind::Success => Chrome::OK,
            ToastKind::Error => Chrome::ERR,
        }
    }
}

pub struct WallpaperStyleApp {
    paths: Paths,
    backend: PlasmaBackend,
    settings: Settings,
    page: Page,
    variant: Variant,

    scan: Option<ScanResult>,
    scanning: bool,
    scanning_started: Option<std::time::Instant>,

    tx: Sender<Worker>,
    rx: Receiver<Worker>,
    busy: Option<String>,

    environment: ws_backend::EnvironmentReport,
    service_state: ws_service::ServiceState,

    toasts: Vec<Toast>,

    /// Text field for adding a custom window-class rule.
    new_rule_class: String,
    new_rule_opacity: f32,
    /// Text field for adding an exclusion.
    new_exclusion: String,
    /// Window class typed on the Identify section.
    identify_input: String,

    snapshots: Vec<ws_core::SnapshotSummary>,
    /// Set while the user is confirming an uninstall.
    confirm_uninstall: bool,
    /// Settings that differ from the file on disk, so the UI can show Dirty.
    dirty: bool,
    /// Accent pinned by the user, if any.
    accent_override: Option<Rgb>,
    background_alpha: u8,
    /// Advanced colour overrides, edited per role.
    overrides: PaletteOverrides,
}

impl WallpaperStyleApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        theme::apply_style(&cc.egui_ctx);
        let paths = Paths::from_env().map_err(|e| e.to_string())?;
        let backend = PlasmaBackend::detect(paths.clone()).map_err(|e| e.to_string())?;
        let (settings, _) = Settings::load(&paths.settings_file()).unwrap_or_default();
        let environment = backend.environment();
        let service_state = ws_service::ServiceState::load(&paths.last_applied_file());
        let snapshots = SnapshotStore::new(paths.snapshots_dir()).list();
        let (tx, rx) = channel();

        let variant = settings.general.variant;
        let background_alpha = settings.palette.background_alpha;
        let accent_override = settings.palette.accent;
        let overrides = settings.palette.overrides.clone();

        let mut app = Self {
            paths,
            backend,
            settings,
            page: Page::Overview,
            variant,
            scan: None,
            scanning: false,
            scanning_started: None,
            tx,
            rx,
            busy: None,
            environment,
            service_state,
            toasts: Vec::new(),
            new_rule_class: String::new(),
            new_rule_opacity: 0.8,
            new_exclusion: String::new(),
            identify_input: String::new(),
            snapshots,
            confirm_uninstall: false,
            dirty: false,
            accent_override,
            background_alpha,
            overrides,
        };
        app.start_scan();
        Ok(app)
    }

    // ----- background work -------------------------------------------------

    fn start_scan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.scanning_started = Some(std::time::Instant::now());
        let tx = self.tx.clone();
        let paths = self.paths.clone();
        let variant = self.variant;
        let alpha = self.background_alpha;
        let overrides = self.overrides.clone();
        let accent = self.accent_override;
        let manual = self.paths.settings_file();

        std::thread::spawn(move || {
            let mut backend = match PlasmaBackend::detect(paths.clone()) {
                Ok(b) => b,
                Err(err) => {
                    let _ = tx.send(Worker::Scanned(ScanResult {
                        wallpaper: None,
                        clusters: Vec::new(),
                        accent: ws_palette::extract::FALLBACK_ACCENT,
                        accent_source: ws_palette::AccentSource::Fallback,
                        dark: fallback_report(Variant::Dark),
                        light: fallback_report(Variant::Light),
                        monochrome: true,
                        slideshow: false,
                        error: Some(err.to_string()),
                    }));
                    return;
                }
            };
            // Honour a manually chosen image, stored in settings.
            if let Ok((settings, _)) = Settings::load(&manual) {
                backend.manual_image = settings.palette.manual_image.clone();
            }

            let detected = backend.detect().ok().flatten();
            let Some(detected) = detected else {
                let _ = tx.send(Worker::Scanned(ScanResult {
                    wallpaper: None,
                    clusters: Vec::new(),
                    accent: ws_palette::extract::FALLBACK_ACCENT,
                    accent_source: ws_palette::AccentSource::Fallback,
                    dark: fallback_report(variant),
                    light: fallback_report(Variant::Light),
                    monochrome: true,
                    slideshow: false,
                    error: Some(
                        "No wallpaper was detected. Choose an image manually on the Palette page."
                            .to_owned(),
                    ),
                }));
                return;
            };

            let result = match ws_palette::sample_path(&detected.path) {
                Ok(sampled) => {
                    let mut overrides = overrides;
                    overrides.accent = accent.or(overrides.accent);
                    let derivation = ws_palette::derive(&sampled, None, alpha, &overrides);
                    ScanResult {
                        wallpaper: Some(detected.path.clone()),
                        clusters: derivation.clusters.clone(),
                        accent: derivation.accent,
                        accent_source: derivation.source,
                        dark: derivation.dark,
                        light: derivation.light,
                        monochrome: sampled.is_monochrome,
                        slideshow: detected.is_slideshow,
                        error: None,
                    }
                }
                Err(err) => ScanResult {
                    wallpaper: Some(detected.path.clone()),
                    clusters: Vec::new(),
                    accent: ws_palette::extract::FALLBACK_ACCENT,
                    accent_source: ws_palette::AccentSource::Fallback,
                    dark: fallback_report(Variant::Dark),
                    light: fallback_report(variant),
                    monochrome: true,
                    slideshow: detected.is_slideshow,
                    error: Some(format!("The wallpaper could not be read: {err}")),
                },
            };
            let _ = tx.send(Worker::Scanned(result));
        });
    }

    fn current_report(&self) -> Option<&PaletteReport> {
        let scan = self.scan.as_ref()?;
        Some(match self.variant {
            Variant::Dark => &scan.dark,
            Variant::Light => &scan.light,
        })
    }

    fn start_apply(&mut self) {
        let Some(report) = self.current_report().cloned() else {
            self.toast("Nothing to apply yet.", ToastKind::Error);
            return;
        };
        self.busy = Some("Applying theme…".to_owned());
        let tx = self.tx.clone();
        let paths = self.paths.clone();
        let settings = self.settings_snapshot();
        let wallpaper = self.scan.as_ref().and_then(|s| s.wallpaper.clone());

        std::thread::spawn(move || {
            let result = (|| -> Result<ApplySummary, String> {
                let backend = PlasmaBackend::detect(paths.clone()).map_err(|e| e.to_string())?;
                let mut ownership = Ownership::load(&paths.ownership_file()).unwrap_or_default();
                let store = SnapshotStore::new(paths.snapshots_dir());

                let mut snapshot = ws_core::Snapshot::empty(
                    store.next_id(ownership.applied_generation),
                    ownership.applied_generation,
                    "gui-apply",
                );
                // Capture the files apply is about to write.
                for path in planned_paths(&backend, &report, &settings, &paths) {
                    let previous = ws_core::snapshot::capture_file(&path)
                        .unwrap_or(ws_core::snapshot::CapturedState::Absent);
                    snapshot.entries.insert(
                        format!("file:{}", path.display()),
                        ws_core::snapshot::SnapshotEntry {
                            id: format!("file:{}", path.display()),
                            previous,
                            applied: ws_core::snapshot::CapturedState::Absent,
                            path,
                            detail: None,
                        },
                    );
                }

                let applied = backend
                    .apply(&report, &settings, &mut ownership)
                    .map_err(|e| e.to_string())?;

                for entry in snapshot.entries.values_mut() {
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
                    store.write(&snapshot).map_err(|e| e.to_string())?;
                    ownership.snapshots.push(snapshot.id.clone());
                    Some(snapshot.id.clone())
                };

                if let Some(wallpaper) = wallpaper {
                    let fingerprint = ws_core::WallpaperFingerprint::of(&wallpaper).ok();
                    let state = ws_service::ServiceState {
                        wallpaper: Some(wallpaper),
                        accent: Some(report.palette.accent.to_hex()),
                        variant: report.variant.as_str().to_owned(),
                        last_applied_unix: Some(ws_core::snapshot::now_unix()),
                        last_error: None,
                        consecutive_failures: 0,
                        wallpaper_fingerprint: fingerprint,
                    };
                    let _ = state.save(&paths.last_applied_file());
                }
                ownership
                    .save(&paths.ownership_file())
                    .map_err(|e| e.to_string())?;

                Ok(ApplySummary {
                    changed: applied.changed,
                    skipped: applied.skipped,
                    restart: applied.restart_required,
                    snapshot: snapshot_id,
                })
            })();
            let _ = tx.send(Worker::Applied(result));
        });
    }

    fn start_restore(&mut self, force: bool) {
        self.busy = Some("Restoring…".to_owned());
        let tx = self.tx.clone();
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let backend = PlasmaBackend::detect(paths.clone()).map_err(|e| e.to_string())?;
                let store = SnapshotStore::new(paths.snapshots_dir());
                let latest = store
                    .list()
                    .into_iter()
                    .next()
                    .ok_or("There are no snapshots yet.")?;
                let snapshot = store.load(&latest.id).map_err(|e| e.to_string())?;
                let entries = backend
                    .restore(&snapshot, force)
                    .map_err(|e| e.to_string())?;
                let restored = entries
                    .iter()
                    .filter(|e| {
                        matches!(
                            e.outcome,
                            ws_core::RestoreOutcome::Restored | ws_core::RestoreOutcome::Removed
                        )
                    })
                    .count();
                let conflicts: Vec<String> = entries
                    .iter()
                    .filter(|e| matches!(e.outcome, ws_core::RestoreOutcome::Conflict))
                    .map(|e| e.item.describe())
                    .collect();
                Ok(if conflicts.is_empty() {
                    format!("Restored {restored} change(s).")
                } else {
                    format!(
                        "Restored {restored} change(s); left {} alone because they changed after \
                         applying.",
                        conflicts.len()
                    )
                })
            })();
            let _ = tx.send(Worker::Restored(result));
        });
    }

    fn start_install(&mut self) {
        self.busy = Some("Installing…".to_owned());
        let tx = self.tx.clone();
        let paths = self.paths.clone();
        // The unit must run the *CLI*, not this GUI: `service run` is a CLI
        // subcommand. Look for a CLI beside this executable so an installed
        // layout works without guessing a PATH.
        let exe = crate::cli_binary();
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let installer = PlasmaInstaller::new(paths.clone(), exe);
                let mut ownership = Ownership::load(&paths.ownership_file()).unwrap_or_default();
                let report = installer
                    .install(&mut ownership)
                    .map_err(|e| e.to_string())?;
                Ok(format!(
                    "Installed {} file(s), {} unchanged.",
                    report.installed.len(),
                    report.unchanged.len()
                ))
            })();
            let _ = tx.send(Worker::Installed(result));
        });
    }

    fn start_uninstall(&mut self) {
        self.busy = Some("Uninstalling…".to_owned());
        let tx = self.tx.clone();
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let backend = PlasmaBackend::detect(paths.clone()).map_err(|e| e.to_string())?;
                let store = SnapshotStore::new(paths.snapshots_dir());
                let mut messages = Vec::new();
                if let Some(latest) = store.list().into_iter().next() {
                    if let Ok(snapshot) = store.load(&latest.id) {
                        let entries = backend
                            .restore(&snapshot, false)
                            .map_err(|e| e.to_string())?;
                        let restored = entries
                            .iter()
                            .filter(|e| {
                                matches!(
                                    e.outcome,
                                    ws_core::RestoreOutcome::Restored
                                        | ws_core::RestoreOutcome::Removed
                                )
                            })
                            .count();
                        messages.push(format!("reverted {restored} theme change(s)"));
                    }
                }
                let installer = PlasmaInstaller::new(paths.clone(), PathBuf::new());
                let entries = installer.uninstall(false).map_err(|e| e.to_string())?;
                messages.push(format!(
                    "removed {} installed file(s)",
                    entries
                        .iter()
                        .filter(|e| matches!(e.outcome, ws_core::RestoreOutcome::Removed))
                        .count()
                ));
                Ok(messages.join("; "))
            })();
            let _ = tx.send(Worker::Restored(
                result.map(|m| format!("Uninstalled: {m}")),
            ));
        });
    }

    // ----- helpers ---------------------------------------------------------

    /// Settings with every UI-widget value folded in, ready to save or apply.
    fn settings_snapshot(&self) -> Settings {
        let mut settings = self.settings.clone();
        settings.general.variant = self.variant;
        settings.palette.background_alpha = self.background_alpha;
        settings.palette.overrides = self.overrides.clone();
        settings.palette.accent = self.accent_override;
        settings.normalised()
    }

    fn save_settings(&mut self) {
        let settings = self.settings_snapshot();
        match settings.save(&self.paths.settings_file()) {
            Ok(()) => {
                self.settings = settings;
                self.dirty = false;
                self.toast("Settings saved.", ToastKind::Success);
            }
            Err(err) => self.toast(format!("Could not save settings: {err}"), ToastKind::Error),
        }
    }

    fn toast(&mut self, message: impl Into<String>, kind: ToastKind) {
        let now = ui_time();
        self.toasts.push(Toast {
            message: message.into(),
            kind,
            born: now,
        });
        // Keep the list short so the window never fills with notifications.
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Worker::Scanned(result) => {
                    self.scanning = false;
                    if let Some(err) = &result.error {
                        self.toast(err.clone(), ToastKind::Error);
                    }
                    self.scan = Some(result);
                }
                Worker::Applied(Ok(summary)) => {
                    self.busy = None;
                    let mut message = match &summary.snapshot {
                        Some(id) => format!(
                            "Applied. {} file(s) changed; undo with snapshot {}.",
                            summary.changed.len(),
                            id
                        ),
                        None => format!("Applied. {} file(s) changed.", summary.changed.len()),
                    };
                    if !summary.skipped.is_empty() {
                        message.push_str(&format!(" {} step(s) skipped.", summary.skipped.len()));
                    }
                    self.toast(message, ToastKind::Success);
                    for (what, why) in &summary.skipped {
                        self.toast(format!("Skipped {what}: {why}"), ToastKind::Info);
                    }
                    for note in &summary.restart {
                        self.toast(note.clone(), ToastKind::Info);
                    }
                    self.service_state =
                        ws_service::ServiceState::load(&self.paths.last_applied_file());
                    self.snapshots = SnapshotStore::new(self.paths.snapshots_dir()).list();
                }
                Worker::Applied(Err(err)) => {
                    self.busy = None;
                    self.toast(format!("Apply failed: {err}"), ToastKind::Error);
                }
                Worker::Restored(Ok(message)) => {
                    self.busy = None;
                    self.toast(message, ToastKind::Success);
                    self.snapshots = SnapshotStore::new(self.paths.snapshots_dir()).list();
                }
                Worker::Restored(Err(err)) => {
                    self.busy = None;
                    self.toast(format!("Failed: {err}"), ToastKind::Error);
                }
                Worker::Installed(Ok(message)) => {
                    self.busy = None;
                    self.toast(message, ToastKind::Success);
                }
                Worker::Installed(Err(err)) => {
                    self.busy = None;
                    self.toast(format!("Install failed: {err}"), ToastKind::Error);
                }
            }
            ctx.request_repaint();
        }
    }

    // ----- pages -----------------------------------------------------------

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("Wallpaper Style")
                        .size(16.0)
                        .strong()
                        .color(Chrome::TEXT),
                );
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(11.0)
                        .color(Chrome::TEXT_FAINT),
                );
            });
        });
        ui.add_space(14.0);

        for page in Page::ALL {
            let selected = self.page == page;
            let (rect, response) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
            if selected {
                ui.painter().rect_filled(
                    rect.shrink2(egui::vec2(8.0, 2.0)),
                    egui::CornerRadius::same(6),
                    Chrome::PANEL_RAISED,
                );
            } else if response.hovered() {
                ui.painter().rect_filled(
                    rect.shrink2(egui::vec2(8.0, 2.0)),
                    egui::CornerRadius::same(6),
                    Color32::from_rgb(0x1a, 0x1d, 0x22),
                );
            }
            ui.painter().text(
                rect.left_center() + egui::vec2(20.0, 0.0),
                egui::Align2::LEFT_CENTER,
                page.icon(),
                egui::FontId::proportional(14.0),
                if selected {
                    Chrome::ACCENT
                } else {
                    Chrome::TEXT_FAINT
                },
            );
            ui.painter().text(
                rect.left_center() + egui::vec2(46.0, 0.0),
                egui::Align2::LEFT_CENTER,
                page.title(),
                egui::FontId::proportional(14.0),
                if selected {
                    Chrome::TEXT
                } else {
                    Chrome::TEXT_DIM
                },
            );
            if response.clicked() {
                self.page = page;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    let (label, state) = if self.environment.supported {
                        ("Plasma 6 · Wayland", PillState::Ok)
                    } else {
                        ("Unsupported session", PillState::Off)
                    };
                    theme::status_pill(ui, label, state, None);
                });
            });
        });
    }

    fn page_overview(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Overview").size(22.0).strong());
        ui.add_space(2.0);
        ui.label(
            RichText::new("What Wallpaper Style is doing right now.")
                .color(Chrome::TEXT_DIM)
                .size(13.0),
        );
        ui.add_space(14.0);

        // Wallpaper + palette.
        egui::Grid::new("overview-top")
            .num_columns(2)
            .spacing(egui::vec2(16.0, 16.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(380.0));
                    theme::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            theme::section(ui, "Wallpaper");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("Rescan").clicked() {
                                    self.start_scan();
                                }
                            });
                        });
                        ui.add_space(4.0);
                        match self.scan.as_ref().and_then(|s| s.wallpaper.clone()) {
                            Some(path) => {
                                ui.label(
                                    RichText::new(shorten_path(&self.paths, &path))
                                        .color(Chrome::TEXT)
                                        .size(13.0),
                                );
                                if self.scan.as_ref().is_some_and(|s| s.slideshow) {
                                    theme::hint(
                                        ui,
                                        "Slideshow detected: the accent follows the current frame.",
                                    );
                                }
                                if self.scan.as_ref().is_some_and(|s| s.monochrome) {
                                    theme::hint(
                                        ui,
                                        "This image has almost no colour, so a fallback accent is used.",
                                    );
                                }
                            }
                            None => {
                                theme::hint(
                                    ui,
                                    "No wallpaper detected yet. Use the Palette page to choose an image.",
                                );
                            }
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            theme::section(ui, "Generated palette");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.selectable_value(
                                    &mut self.variant,
                                    Variant::Light,
                                    "Light",
                                );
                                ui.selectable_value(&mut self.variant, Variant::Dark, "Dark");
                            });
                        });
                        ui.add_space(6.0);
                        if let Some(report) = self.current_report() {
                            let colors = theme::PreviewColors::from_palette(&report.palette);
                            theme::window_preview(ui, &colors, true);
                            ui.add_space(6.0);
                            let worst = report.worst_text_ratio();
                            let state = if worst >= 4.5 { PillState::Ok } else { PillState::Warn };
                            theme::status_pill(
                                ui,
                                &format!("Text contrast {worst:.1}:1"),
                                state,
                                Some("needs 4.5:1"),
                            );
                        } else {
                            theme::hint(ui, "Working…");
                        }
                    });
                });

                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(380.0));
                    theme::card(ui, |ui| {
                        theme::section(ui, "Service");
                        ui.add_space(4.0);
                        let installed = self.paths.data_dir.join("installed.json").exists();
                        theme::status_pill(
                            ui,
                            if installed { "Installed" } else { "Not installed" },
                            if installed { PillState::Ok } else { PillState::Warn },
                            None,
                        );
                        theme::status_pill(
                            ui,
                            if self.settings.general.automatic {
                                "Automatic updates on"
                            } else {
                                "Automatic updates off"
                            },
                            if self.settings.general.automatic { PillState::Ok } else { PillState::Neutral },
                            None,
                        );
                        if let Some(error) = &self.service_state.last_error {
                            theme::status_pill(ui, "Last run failed", PillState::Off, None);
                            theme::hint(ui, error);
                        } else if let Some(ts) = self.service_state.last_applied_unix {
                            theme::status_pill(
                                ui,
                                &format!("Last applied {}", relative_time(ts)),
                                PillState::Ok,
                                None,
                            );
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Apply now").clicked() {
                                self.start_apply();
                            }
                            if ui.button("Install service").clicked() {
                                self.start_install();
                            }
                        });
                    });
                    ui.add_space(12.0);
                    theme::card(ui, |ui| {
                        theme::section(ui, "Availability");
                        ui.add_space(4.0);
                        for feature in Feature::ALL {
                            let availability = self.environment.availability(feature);
                            let (state, detail) = match &availability {
                                Availability::Available => (PillState::Ok, None),
                                Availability::Degraded { reason } => (PillState::Warn, Some(reason.as_str())),
                                Availability::Unavailable { reason } => (PillState::Off, Some(reason.as_str())),
                            };
                            theme::status_pill(ui, feature.label(), state, detail);
                        }
                    });
                });
            });
    }

    fn page_palette(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Palette").size(22.0).strong());
        ui.label(
            RichText::new("Where the colours come from, and how to override them.")
                .color(Chrome::TEXT_DIM)
                .size(13.0),
        );
        ui.add_space(14.0);

        ui.horizontal(|ui| {
            ui.label("Variant");
            if ui
                .selectable_value(&mut self.variant, Variant::Dark, "Dark")
                .clicked()
            {
                self.mark_dirty();
            }
            if ui
                .selectable_value(&mut self.variant, Variant::Light, "Light")
                .clicked()
            {
                self.mark_dirty();
            }
            ui.add_space(20.0);
            if ui.button("Choose an image…").clicked() {
                if let Some(path) = rfd_pick_image() {
                    self.settings.palette.manual_image = Some(path);
                    self.mark_dirty();
                    self.save_settings();
                    self.start_scan();
                }
            }
            if self.settings.palette.manual_image.is_some()
                && ui.button("Use desktop wallpaper").clicked()
            {
                self.settings.palette.manual_image = None;
                self.mark_dirty();
                self.save_settings();
                self.start_scan();
            }
        });
        ui.add_space(10.0);

        egui::Grid::new("palette-grid")
            .num_columns(2)
            .spacing(egui::vec2(16.0, 16.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(400.0));
                    theme::card(ui, |ui| {
                        theme::section(ui, "Colour clusters");
                        theme::hint(ui, "How much of the image each hue explains.");
                        ui.add_space(6.0);
                        match self.scan.as_ref() {
                            Some(scan) if !scan.clusters.is_empty() => {
                                for cluster in scan.clusters.iter().take(6) {
                                    let color = ws_palette::Oklch {
                                        l: 0.62,
                                        c: cluster.chroma.max(0.06),
                                        h: cluster.hue,
                                    }
                                    .to_rgb();
                                    ui.horizontal(|ui| {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(14.0, 14.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().rect_filled(
                                            rect,
                                            egui::CornerRadius::same(3),
                                            theme::to_color32(color),
                                        );
                                        ui.label(
                                            RichText::new(format!("{:.0}°", cluster.hue))
                                                .monospace()
                                                .size(12.0),
                                        );
                                        ui.add(
                                            egui::ProgressBar::new(cluster.weight)
                                                .desired_width(120.0)
                                                .fill(theme::to_color32(color))
                                                .text(
                                                    RichText::new(format!(
                                                        "{:.0}%",
                                                        cluster.weight * 100.0
                                                    ))
                                                    .size(10.0),
                                                ),
                                        );
                                    });
                                }
                                ui.add_space(6.0);
                                if let Some(scan) = &self.scan {
                                    ui.horizontal(|ui| {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(14.0, 14.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().rect_filled(
                                            rect,
                                            egui::CornerRadius::same(3),
                                            theme::to_color32(scan.accent),
                                        );
                                        theme::hint(
                                            ui,
                                            &format!(
                                                "Accent {} from {:?}",
                                                scan.accent.to_hex(),
                                                scan.accent_source
                                            ),
                                        );
                                    });
                                }
                            }
                            _ => theme::hint(
                                ui,
                                "No colour clusters (monochrome image or not loaded).",
                            ),
                        }
                    });
                });

                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(400.0));
                    theme::card(ui, |ui| {
                        theme::section(ui, "Palette colours");
                        ui.add_space(4.0);
                        if let Some(report) = self.current_report().cloned() {
                            let p = report.palette;
                            theme::swatch(ui, "Accent", p.accent);
                            theme::swatch(ui, "Background", p.background);
                            theme::swatch(ui, "Surface", p.surface);
                            theme::swatch(ui, "Button", p.button);
                            theme::swatch(ui, "Hover", p.hover);
                            theme::swatch(ui, "Selection", p.selection);
                            theme::swatch(ui, "Text", p.text);
                            theme::swatch(ui, "Warning", p.warning);
                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(6.0);
                            theme::section(ui, "Contrast");
                            for check in &report.checks {
                                let state = if check.passes() {
                                    PillState::Ok
                                } else {
                                    PillState::Off
                                };
                                theme::status_pill(
                                    ui,
                                    &format!(
                                        "{} on {} — {:.1}:1",
                                        check.foreground_href, check.background_href, check.ratio
                                    ),
                                    state,
                                    Some(&format!("needs {:.1}", check.required)),
                                );
                            }
                        } else {
                            theme::hint(ui, "Working…");
                        }
                    });
                });
            });

        ui.add_space(14.0);
        theme::card(ui, |ui| {
            theme::section(ui, "Manual override");
            theme::hint(
                ui,
                "Overrides replace the generated colour but are still checked for readability.",
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Accent");
                let mut rgb = self
                    .accent_override
                    .or_else(|| self.current_report().map(|r| r.palette.accent))
                    .unwrap_or(Rgb::rgb(0x7c, 0x8d, 0xf0));
                let mut changed = false;
                ui.horizontal(|ui| {
                    let mut r = rgb.r as f32;
                    let mut g = rgb.g as f32;
                    let mut b = rgb.b as f32;
                    changed |= ui.color_edit_button_srgb(&mut [0, 0, 0]).changed();
                    ui.add(egui::Slider::new(&mut r, 0.0..=255.0).show_value(false));
                    ui.add(egui::Slider::new(&mut g, 0.0..=255.0).show_value(false));
                    ui.add(egui::Slider::new(&mut b, 0.0..=255.0).show_value(false));
                    let new = Rgb::rgb(r as u8, g as u8, b as u8);
                    if new != rgb {
                        rgb = new;
                        changed = true;
                    }
                });
                if changed {
                    self.accent_override = Some(rgb);
                    self.mark_dirty();
                    self.start_scan();
                }
                if ui.button("Reset").clicked() {
                    self.accent_override = None;
                    self.overrides = PaletteOverrides::default();
                    self.mark_dirty();
                    self.start_scan();
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Background opacity");
                let mut alpha = self.background_alpha;
                if ui
                    .add(
                        egui::Slider::new(&mut alpha, 140..=255)
                            .custom_formatter(|v, _| format!("{:.0}%", v / 255.0 * 100.0)),
                    )
                    .changed()
                {
                    self.background_alpha = alpha;
                    self.mark_dirty();
                }
            });
            theme::hint(
                ui,
                "Only the window background becomes translucent; text stays fully opaque. \
                 This needs a Kvantum theme that supports translucency.",
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Save changes").clicked() {
                    self.save_settings();
                    self.start_scan();
                }
                if ui.button("Preview changes").clicked() {
                    self.start_scan();
                }
            });
        });
    }

    fn page_theme(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Theme").size(22.0).strong());
        ui.label(
            RichText::new("Which integrations to generate, and what applying will change.")
                .color(Chrome::TEXT_DIM)
                .size(13.0),
        );
        ui.add_space(14.0);

        egui::Grid::new("theme-grid")
            .num_columns(2)
            .spacing(egui::vec2(16.0, 16.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(400.0));
                    theme::card(ui, |ui| {
                        theme::section(ui, "Integrations");
                        ui.add_space(4.0);
                        let mut changed = false;
                        changed |= checkbox_row(
                            ui,
                            &mut self.settings.themes.plasma_color_scheme,
                            "Plasma colour scheme",
                            "Windows, widgets and the panel follow the wallpaper.",
                            self.environment
                                .availability(Feature::ColorScheme)
                                .is_available(),
                        );
                        changed |= checkbox_row(
                            ui,
                            &mut self.settings.themes.kvantum,
                            "Qt applications (Kvantum)",
                            "Generates a theme from your selected base theme.",
                            self.environment
                                .availability(Feature::KvantumTheme)
                                .is_available(),
                        );
                        ui.indent("kvantum-sub", |ui| {
                            changed |= checkbox_row(
                                ui,
                                &mut self.settings.themes.kvantum_glass_variant,
                                "Glass variant",
                                "Background-only transparency for supported Qt applications.",
                                self.settings.themes.kvantum,
                            );
                        });
                        changed |= checkbox_row(
                            ui,
                            &mut self.settings.themes.kwin_effect,
                            "Window transparency (KWin)",
                            "Per-application opacity through a scoped compositor effect.",
                            self.environment
                                .availability(Feature::WindowOpacity)
                                .is_available(),
                        );
                        if changed {
                            self.mark_dirty();
                        }
                    });
                    ui.add_space(12.0);
                    theme::card(ui, |ui| {
                        theme::section(ui, "Behaviour");
                        ui.add_space(4.0);
                        let mut changed = false;
                        changed |= checkbox_row(
                            ui,
                            &mut self.settings.general.automatic,
                            "Update automatically",
                            "Re-theme when the wallpaper changes.",
                            true,
                        );
                        if changed {
                            self.mark_dirty();
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Apply now").clicked() {
                                self.start_apply();
                            }
                            if ui.button("Save changes").clicked() {
                                self.save_settings();
                            }
                        });
                    });
                });

                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(400.0));
                    theme::card(ui, |ui| {
                        theme::section(ui, "Preview");
                        ui.add_space(4.0);
                        match self.current_report() {
                            Some(report) => {
                                let colors = theme::PreviewColors::from_palette(&report.palette);
                                theme::window_preview(ui, &colors, true);
                                ui.add_space(10.0);
                                let settings = self.settings_snapshot();
                                let preview = self.backend.preview(report, &settings);
                                ui.label(
                                    RichText::new("Applying will:")
                                        .color(Chrome::TEXT_DIM)
                                        .size(12.0),
                                );
                                for line in &preview.would_change {
                                    ui.label(
                                        RichText::new(format!("• {line}"))
                                            .color(Chrome::TEXT)
                                            .size(12.0),
                                    );
                                }
                                if !preview.would_skip.is_empty() {
                                    ui.add_space(6.0);
                                    ui.label(
                                        RichText::new("and skip:")
                                            .color(Chrome::TEXT_FAINT)
                                            .size(12.0),
                                    );
                                    for (what, why) in &preview.would_skip {
                                        ui.label(
                                            RichText::new(format!("• {what}: {why}"))
                                                .color(Chrome::TEXT_FAINT)
                                                .size(12.0),
                                        );
                                    }
                                }
                            }
                            None => theme::hint(ui, "Working…"),
                        }
                    });
                });
            });

        ui.add_space(12.0);
        theme::card(ui, |ui| {
            theme::hint(
                ui,
                "Applications restart: Dolphin, Kate, Ark and System Settings reload the Kvantum \
                 theme when reopened. Electron applications (Claude, Codex, VS Code, Cursor, \
                 Heroic) and browsers do not use Kvantum and are handled by window transparency \
                 instead.",
            );
        });
    }

    fn page_transparency(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Transparency").size(22.0).strong());
        ui.label(
            RichText::new("Per-application window opacity, applied by a scoped KWin effect.")
                .color(Chrome::TEXT_DIM)
                .size(13.0),
        );
        ui.add_space(14.0);

        let availability = self.environment.availability(Feature::WindowOpacity);
        if !availability.is_available() || availability.is_degraded() {
            theme::card(ui, |ui| {
                let state = if availability.is_available() {
                    PillState::Warn
                } else {
                    PillState::Off
                };
                theme::status_pill(ui, "Limited on this system", state, None);
                if let Some(reason) = availability.reason() {
                    theme::hint(ui, reason);
                }
            });
            ui.add_space(10.0);
        }

        theme::card(ui, |ui| {
            let mut changed = false;
            changed |= ui
                .checkbox(
                    &mut self.settings.transparency.enabled,
                    RichText::new("Enable per-application transparency").size(14.0),
                )
                .changed();
            theme::hint(
                ui,
                "The effect never moves windows between desktops and never touches panels, \
                 notifications or VirtualBox guest windows.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Default opacity");
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.settings.transparency.default_opacity,
                            0.2..=1.0,
                        )
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("Leave fullscreen windows alone");
                changed |= ui
                    .checkbox(&mut self.settings.transparency.skip_fullscreen, "")
                    .changed();
            });
            if changed {
                self.mark_dirty();
            }
        });

        ui.add_space(12.0);
        ui.label(RichText::new("Applications").size(15.0).strong());
        ui.add_space(4.0);

        let mut changed = false;
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for preset in presets::PRESETS {
                    let entry = self.settings.transparency.apps.get(preset.id);
                    let mut enabled = entry.map(|e| e.enabled).unwrap_or(false);
                    let mut opacity = entry.map(|e| e.opacity).unwrap_or(preset.default_opacity);

                    theme::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .checkbox(&mut enabled, RichText::new(preset.name).strong())
                                .changed()
                            {
                                if enabled {
                                    let mut app = AppOpacity::new(opacity);
                                    app.enabled = true;
                                    self.settings
                                        .transparency
                                        .apps
                                        .insert(preset.id.to_owned(), app);
                                } else {
                                    self.settings.transparency.apps.remove(preset.id);
                                }
                                changed = true;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let kind = match preset.kind {
                                        ws_core::TransparencyKind::WholeWindow => "whole window",
                                        ws_core::TransparencyKind::BackgroundOnly => {
                                            "background only"
                                        }
                                    };
                                    ui.label(
                                        RichText::new(kind).color(Chrome::TEXT_FAINT).size(11.0),
                                    );
                                },
                            );
                        });
                        if enabled {
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::Slider::new(&mut opacity, 0.2..=1.0)
                                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                                    )
                                    .changed()
                                {
                                    if let Some(app) =
                                        self.settings.transparency.apps.get_mut(preset.id)
                                    {
                                        app.opacity = opacity;
                                    }
                                    changed = true;
                                }
                            });
                        }
                        theme::hint(ui, preset.note);
                    });
                    ui.add_space(6.0);
                }
            });
        if changed {
            self.mark_dirty();
        }

        ui.add_space(10.0);
        egui::Grid::new("transparency-extra")
            .num_columns(2)
            .spacing(egui::vec2(16.0, 12.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(380.0));
                    theme::card(ui, |ui| {
                        theme::section(ui, "Custom rules");
                        theme::hint(ui, "Add any window class, for applications without a preset.");
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.new_rule_class)
                                    .hint_text("window class, e.g. alacritty")
                                    .desired_width(180.0),
                            );
                            ui.add(
                                egui::Slider::new(&mut self.new_rule_opacity, 0.2..=1.0)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                            );
                            if ui.button("Add").clicked() && !self.new_rule_class.trim().is_empty() {
                                self.settings.transparency.custom.insert(
                                    self.new_rule_class.trim().to_owned(),
                                    AppOpacity::new(self.new_rule_opacity),
                                );
                                self.new_rule_class.clear();
                                self.mark_dirty();
                            }
                        });
                        ui.add_space(6.0);
                        let mut remove = None;
                        for (class, app) in &self.settings.transparency.custom {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(class).monospace().size(12.0));
                                ui.label(
                                    RichText::new(format!("{:.0}%", app.opacity * 100.0))
                                        .color(Chrome::TEXT_FAINT)
                                        .size(12.0),
                                );
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("Remove").clicked() {
                                        remove = Some(class.clone());
                                    }
                                });
                            });
                        }
                        if let Some(class) = remove {
                            self.settings.transparency.custom.remove(&class);
                            self.mark_dirty();
                        }
                    });
                });

                ui.vertical(|ui| {
                    ui.set_width(ui.available_width().min(380.0));
                    theme::card(ui, |ui| {
                        theme::section(ui, "Identify an application");
                        theme::hint(
                            ui,
                            "Type a window class, or use KWin's window information tool to copy one.",
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.identify_input)
                                    .hint_text("com.anthropic.Claude")
                                    .desired_width(200.0),
                            );
                            if ui.button("Add rule").clicked() && !self.identify_input.trim().is_empty() {
                                self.settings.transparency.custom.insert(
                                    self.identify_input.trim().to_owned(),
                                    AppOpacity::new(0.85),
                                );
                                self.identify_input.clear();
                                self.mark_dirty();
                            }
                        });
                        ui.add_space(6.0);
                        let info = presets::identify(&self.identify_input);
                        if !self.identify_input.trim().is_empty() {
                            match info.preset {
                                Some(id) => {
                                    let preset = presets::preset(id).expect("id from catalogue");
                                    theme::status_pill(ui, &format!("Matches {}", preset.name), PillState::Ok, None);
                                    theme::hint(ui, preset.note);
                                }
                                None => theme::status_pill(ui, "No preset matches", PillState::Neutral, None),
                            }
                            if let Some(reason) = info.excluded_reason {
                                theme::status_pill(
                                    ui,
                                    "Excluded from transparency",
                                    PillState::Off,
                                    Some(reason.describe()),
                                );
                            }
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        theme::section(ui, "Exclusions");
                        theme::hint(ui, "Window classes that are never made transparent.");
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.new_exclusion)
                                    .hint_text("window class")
                                    .desired_width(180.0),
                            );
                            if ui.button("Exclude").clicked() && !self.new_exclusion.trim().is_empty() {
                                self.settings.transparency.exclusions.push(self.new_exclusion.trim().to_owned());
                                self.new_exclusion.clear();
                                self.mark_dirty();
                            }
                        });
                        let mut remove = None;
                        for class in &self.settings.transparency.exclusions {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(class).monospace().size(12.0));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("Remove").clicked() {
                                        remove = Some(class.clone());
                                    }
                                });
                            });
                        }
                        if let Some(class) = remove {
                            self.settings.transparency.exclusions.retain(|c| *c != class);
                            self.mark_dirty();
                        }
                    });
                });
            });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Save changes").clicked() {
                self.save_settings();
            }
            if ui.button("Apply transparency").clicked() {
                self.save_settings();
                self.start_apply();
            }
        });
    }

    fn page_recovery(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Recovery").size(22.0).strong());
        ui.label(
            RichText::new("A snapshot is taken before every apply, so anything can be undone.")
                .color(Chrome::TEXT_DIM)
                .size(13.0),
        );
        ui.add_space(14.0);

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section(ui, "Snapshots");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        self.snapshots = SnapshotStore::new(self.paths.snapshots_dir()).list();
                    }
                });
            });
            ui.add_space(6.0);
            if self.snapshots.is_empty() {
                theme::hint(
                    ui,
                    "No snapshots yet. One is taken automatically before each apply.",
                );
            } else {
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for summary in &self.snapshots {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&summary.id)
                                        .monospace()
                                        .size(12.0)
                                        .color(Chrome::TEXT),
                                );
                                ui.label(
                                    RichText::new(format!("{} change(s)", summary.entries))
                                        .size(12.0)
                                        .color(Chrome::TEXT_FAINT),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(&summary.reason)
                                                .size(11.0)
                                                .color(Chrome::TEXT_FAINT),
                                        );
                                    },
                                );
                            });
                        }
                    });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Restore last snapshot").clicked() {
                    self.start_restore(false);
                }
                theme::hint(
                    ui,
                    "Leaves values alone that changed after the snapshot was taken.",
                );
            });
        });

        ui.add_space(12.0);
        theme::card(ui, |ui| {
            theme::section(ui, "What Wallpaper Style owns");
            ui.add_space(4.0);
            let ownership = Ownership::load(&self.paths.ownership_file()).unwrap_or_default();
            if ownership.items.is_empty() {
                theme::hint(ui, "Nothing recorded yet.");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for item in ownership.items.values() {
                            ui.label(
                                RichText::new(item.describe())
                                    .monospace()
                                    .size(11.0)
                                    .color(Chrome::TEXT_DIM),
                            );
                        }
                    });
            }
            ui.add_space(6.0);
            theme::hint(
                ui,
                "Restore only undoes these entries. Your own colour scheme, Kvantum theme and \
                 window settings are never touched.",
            );
        });

        ui.add_space(12.0);
        theme::card(ui, |ui| {
            theme::section(ui, "Uninstall");
            theme::hint(
                ui,
                "Stops the background service, removes the KWin effect and reverts theme changes. \
                 Settings and snapshots are kept so you can reinstall later.",
            );
            ui.add_space(8.0);
            if self.confirm_uninstall {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Remove the service and revert the theme?")
                            .color(Chrome::WARN)
                            .size(13.0),
                    );
                    if ui.button("Yes, uninstall").clicked() {
                        self.confirm_uninstall = false;
                        self.start_uninstall();
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_uninstall = false;
                    }
                });
            } else {
                if ui.button("Uninstall…").clicked() {
                    self.confirm_uninstall = true;
                }
                if ui.button("Install or repair service").clicked() {
                    self.start_install();
                }
            }
        });

        ui.add_space(12.0);
        theme::hint(
            ui,
            &format!(
                "Settings: {}\nState: {}",
                self.paths.display(&self.paths.settings_file()),
                self.paths.display(&self.paths.state_dir)
            ),
        );
    }
}

impl eframe::App for WallpaperStyleApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Polling happens here so it also runs while the window is hidden.
        self.poll_worker(ctx);
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;

        // Keep the spinner alive while work is in flight.
        if self.scanning || self.busy.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::Panel::top("titlebar")
            .exact_size(46.0)
            .frame(
                egui::Frame::new()
                    .fill(Chrome::PANEL)
                    .inner_margin(egui::Margin::symmetric(16, 0)),
            )
            .show(root, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new(if self.dirty {
                            "Unsaved changes"
                        } else {
                            "Up to date"
                        })
                        .color(if self.dirty {
                            Chrome::WARN
                        } else {
                            Chrome::TEXT_FAINT
                        })
                        .size(12.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.busy.is_some() {
                            ui.spinner();
                            ui.label(
                                RichText::new(self.busy.as_deref().unwrap_or_default())
                                    .color(Chrome::TEXT_DIM)
                                    .size(12.0),
                            );
                        } else {
                            if ui.button("Apply").clicked() {
                                self.start_apply();
                            }
                            if ui.button("Rescan").clicked() {
                                self.start_scan();
                            }
                        }
                    });
                });
            });

        egui::Panel::left("sidebar")
            .exact_size(208.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(Chrome::PANEL)
                    .inner_margin(egui::Margin::same(0)),
            )
            .show(root, |ui| self.sidebar(ui));

        egui::Panel::bottom("toasts")
            .exact_size(if self.toasts.is_empty() {
                8.0
            } else {
                34.0 * self.toasts.len() as f32 + 8.0
            })
            .frame(
                egui::Frame::new()
                    .fill(Chrome::BG)
                    .inner_margin(egui::Margin::symmetric(16, 4)),
            )
            .show(root, |ui| {
                let now = ui_time();
                // Expire old messages so the window does not accumulate them.
                self.toasts
                    .retain(|t| now - t.born < 12.0 || t.kind == ToastKind::Error);
                for toast in &self.toasts {
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                        ui.painter()
                            .circle_filled(rect.center(), 3.0, toast.color());
                        ui.label(
                            RichText::new(&toast.message)
                                .color(toast.color())
                                .size(12.0),
                        );
                    });
                }
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Chrome::BG)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(root, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Overview => self.page_overview(ui),
                    Page::Palette => self.page_palette(ui),
                    Page::Theme => self.page_theme(ui),
                    Page::Transparency => self.page_transparency(ui),
                    Page::Recovery => self.page_recovery(ui),
                });
            });
    }
}

fn checkbox_row(
    ui: &mut egui::Ui,
    value: &mut bool,
    title: &str,
    description: &str,
    available: bool,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(available, |ui| {
            changed = ui.checkbox(value, RichText::new(title)).changed();
        });
        if !available {
            ui.label(
                RichText::new("unavailable")
                    .color(Chrome::TEXT_FAINT)
                    .size(11.0),
            );
        }
    });
    theme::hint(ui, description);
    ui.add_space(4.0);
    changed
}

fn fallback_report(variant: Variant) -> PaletteReport {
    ws_palette::build(ws_palette::extract::FALLBACK_ACCENT, variant, 255)
}

fn shorten_path(paths: &Paths, path: &std::path::Path) -> String {
    paths.display(path)
}

fn relative_time(then: u64) -> String {
    let now = ws_core::snapshot::now_unix();
    let delta = now.saturating_sub(then);
    match delta {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", delta / 60),
        3600..=86399 => format!("{} h ago", delta / 3600),
        _ => format!("{} d ago", delta / 86400),
    }
}

/// Monotonic seconds, used only for toast lifetimes.
fn ui_time() -> f64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs_f64()
}

/// Pick an image through the desktop's file chooser.
///
/// Uses the XDG portal through `rfd` so the picker matches the rest of the
/// desktop and works under Wayland without a sandbox escape.
fn rfd_pick_image() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter(
            "Images",
            &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff"],
        )
        .set_title("Choose a wallpaper image")
        .pick_file()
}

/// Files an apply writes, for the snapshot. Mirrors the backend's layout.
fn planned_paths(
    backend: &PlasmaBackend,
    report: &PaletteReport,
    settings: &Settings,
    paths: &Paths,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if settings.themes.plasma_color_scheme {
        let scheme = ws_plasma::colorscheme::generate(&report.palette, report.variant);
        out.push(backend.color_scheme_dir().join(scheme.file_name()));
    }
    if settings.themes.kwin_effect {
        out.push(backend.effect_dir().join("metadata.json"));
        out.push(backend.effect_dir().join("contents/code/main.js"));
        out.push(paths.kwinrc());
    }
    if settings.themes.kvantum {
        out.push(paths.kvantum_dir().join("kvantum.kvconfig"));
    }
    out
}
