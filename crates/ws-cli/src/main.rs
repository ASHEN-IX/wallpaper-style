//! `wallpaper-style` — command-line interface and diagnostic surface.
//!
//! Every action the GUI can perform is available here, so automation and
//! troubleshooting never require a display server. Output is plain text by
//! default and JSON with `--json`, which the GUI and tests use.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use ws_backend::{Feature, Installer, ThemeApplier, WallpaperSource};
use ws_core::ownership::Ownership;
use ws_core::snapshot::{Snapshot, SnapshotStore};
use ws_core::{presets, Paths, Settings};
use ws_plasma::backend::PlasmaBackend;
use ws_plasma::env::{self, Runner, SystemRunner};
use ws_plasma::install::PlasmaInstaller;
use ws_service::{Sleeper, Tick, Watcher};

/// Wallpaper Style: a desktop appearance that follows your wallpaper.
#[derive(Debug, Parser)]
#[command(
    name = "wallpaper-style",
    version,
    about,
    long_about = "Wallpaper Style derives a colour palette from your wallpaper and applies a \
                  matching KDE Plasma colour scheme, a Kvantum theme for Qt applications, and \
                  scoped per-application window transparency.\n\n\
                  Run `wallpaper-style status` for a diagnosis of this machine.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Emit machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    /// Show more detail in logs.
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report the environment, detected wallpaper, palette and service state.
    Status(StatusArgs),
    /// Extract a palette and show it without changing anything.
    Preview(PreviewArgs),
    /// Apply the generated theme. Takes a snapshot first.
    Apply(ApplyArgs),
    /// Revert everything this application changed.
    Restore(RestoreArgs),
    /// Install the user service and KWin effect.
    Install(InstallArgs),
    /// Remove the service and effect, restoring prior settings.
    Uninstall(RestoreArgs),
    /// Read or write settings.
    Settings(SettingsArgs),
    /// Manage snapshots.
    Snapshot(SnapshotArgs),
    /// List application presets and explain their limits.
    Presets,
    /// Identify an application by its window class.
    Identify { window_class: String },
    /// Run the background watcher in the foreground.
    Service(ServiceArgs),
    /// Print what is installed and what depends on it.
    Doctor,
    /// Write an SVG showing the palette as a specimen window.
    ///
    /// Useful for documentation and for sharing a look without screenshots.
    ExportPreview(ExportArgs),
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Where to write the SVG. Defaults to stdout.
    #[arg(long, short, value_name = "PATH")]
    output: Option<PathBuf>,
    /// Use this image instead of the detected wallpaper.
    #[arg(long, value_name = "PATH")]
    image: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Include the full contrast report.
    #[arg(long)]
    detailed: bool,
}

#[derive(Debug, Args)]
struct PreviewArgs {
    /// Use this image instead of the detected wallpaper.
    #[arg(long, value_name = "PATH")]
    image: Option<PathBuf>,
    /// Show the generated colour scheme file instead of a swatch table.
    #[arg(long)]
    scheme: bool,
}

#[derive(Debug, Args)]
struct ApplyArgs {
    /// Use this image instead of the detected wallpaper.
    #[arg(long, value_name = "PATH")]
    image: Option<PathBuf>,
    /// Apply even if the palette would be identical (normally a no-op).
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct RestoreArgs {
    /// Overwrite values that changed since the snapshot was taken.
    #[arg(long)]
    force: bool,
    /// Restore this snapshot instead of the most recent one.
    #[arg(long, value_name = "ID")]
    snapshot: Option<String>,
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// Do not enable the systemd unit; only write files.
    #[arg(long)]
    no_enable: bool,
}

#[derive(Debug, Args)]
struct SettingsArgs {
    /// Print the effective settings as TOML.
    #[arg(long)]
    show: bool,
    /// Set a value, e.g. --set general.automatic=true
    #[arg(long, value_name = "KEY=VALUE")]
    set: Vec<String>,
    /// Import the Python prototype's settings.json.
    #[arg(long, value_name = "PATH")]
    import_legacy: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SnapshotArgs {
    #[command(subcommand)]
    action: SnapshotAction,
}

#[derive(Debug, Subcommand)]
enum SnapshotAction {
    /// List snapshots, newest first.
    List,
    /// Show one snapshot's contents.
    Show { id: String },
}

#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    action: ServiceAction,
}

#[derive(Debug, Subcommand)]
enum ServiceAction {
    /// Run the watcher in the foreground until interrupted.
    Run,
    /// Run a single tick and exit; useful for tests and timers.
    Once,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` prints the whole context chain on one line, which is what a
            // user needs from a CLI.
            eprintln!("wallpaper-style: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbosity: u8) {
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    // The GUI and systemd both want logs on stderr; never write to stdout, which
    // carries the machine-readable output.
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| format!("{level},ws_palette=warn"));
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter))
        .target(env_logger::Target::Stderr)
        .try_init();
}

fn paths() -> Result<Paths> {
    Paths::from_env().context("cannot determine the user's configuration directories")
}

fn backend(paths: &Paths) -> Result<PlasmaBackend> {
    PlasmaBackend::detect(paths.clone()).context("cannot start the Plasma backend")
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.command {
        Command::Status(args) => status(json, args),
        Command::Preview(args) => preview(json, args),
        Command::Apply(args) => apply(json, args),
        Command::Restore(args) => restore(json, args),
        Command::Install(args) => install(json, args),
        Command::Uninstall(args) => uninstall(json, args),
        Command::Settings(args) => settings_command(json, args),
        Command::Snapshot(args) => snapshot_command(json, args),
        Command::Presets => presets_command(json),
        Command::Identify { window_class } => identify_command(json, &window_class),
        Command::Service(args) => service_command(json, args),
        Command::Doctor => doctor(json),
        Command::ExportPreview(args) => export_preview(args),
    }
}

/// Render the palette as a self-contained SVG specimen.
///
/// Shapes only, no text outlines, so any viewer renders it identically and it
/// stays small. The layout mirrors the GUI's preview panel so the documentation
/// image matches what users actually see.
fn export_preview(args: ExportArgs) -> Result<()> {
    let paths = paths()?;
    let backend = backend(&paths)?;
    let settings = Settings::load(&paths.settings_file())
        .map(|(s, _)| s)
        .unwrap_or_default();
    let image = match &args.image {
        Some(path) => path.clone(),
        None => backend
            .detect()
            .context("cannot detect the wallpaper")?
            .map(|d| d.path)
            .context("no wallpaper was detected; pass --image PATH")?,
    };
    let sampled = ws_palette::sample_path(&image)
        .with_context(|| format!("cannot read {}", image.display()))?;
    let derivation = ws_palette::derive(
        &sampled,
        None,
        settings.palette.background_alpha,
        &settings.palette.overrides,
    );
    let report = match settings.general.variant {
        ws_palette::Variant::Dark => derivation.dark,
        ws_palette::Variant::Light => derivation.light,
    };

    let svg = ws_plasma::colorscheme::preview_svg(&report.palette, report.variant);
    match &args.output {
        Some(path) => {
            std::fs::write(path, &svg)
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("Wrote {}", paths.display(path));
        }
        None => print!("{svg}"),
    }
    Ok(())
}

fn out(json: bool, value: serde_json::Value, text: impl FnOnce()) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        text();
    }
}

fn status(json: bool, args: StatusArgs) -> Result<()> {
    let paths = paths()?;
    let backend = backend(&paths)?;
    let environment = backend.environment();
    let detected = backend.detect().ok().flatten();
    let settings = Settings::load(&paths.settings_file())
        .map(|(s, _)| s)
        .unwrap_or_default();
    let state = ws_service::ServiceState::load(&paths.last_applied_file());
    let ownership = Ownership::load(&paths.ownership_file()).unwrap_or_default();

    let mut palette_report = None;
    if let Some(detected) = &detected {
        if let Ok(sampled) = ws_palette::sample_path(&detected.path) {
            let derivation = ws_palette::derive(
                &sampled,
                None,
                settings.palette.background_alpha,
                &settings.palette.overrides,
            );
            palette_report = Some(match settings.general.variant {
                ws_palette::Variant::Dark => derivation.dark,
                ws_palette::Variant::Light => derivation.light,
            });
        }
    }

    if json {
        let payload = serde_json::json!({
            "environment": environment,
            "supported": environment.supported,
            "wallpaper": detected.as_ref().map(|d| serde_json::json!({
                "path": d.path,
                "screen": d.screen,
                "plugin": d.plugin,
                "slideshow": d.is_slideshow,
            })),
            "palette": palette_report,
            "service": state,
            "settings": {
                "automatic": settings.general.automatic,
                "variant": settings.general.variant,
                "transparency": settings.transparency.enabled,
                "kwin_effect": settings.themes.kwin_effect,
            },
            "owned_items": ownership.items.len(),
            "installed": paths.data_dir.join("installed.json").exists(),
        });
        out(true, payload, || {});
        return Ok(());
    }

    println!("Wallpaper Style {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Session");
    println!(
        "  Desktop              {}",
        if environment.desktop.is_empty() {
            "(unknown)"
        } else {
            &environment.desktop
        }
    );
    println!("  Session type         {}", environment.session_type);
    if let Some(version) = &environment.desktop_version {
        println!("  KWin                 {version}");
    }
    if !environment.supported {
        println!();
        println!(
            "  NOT SUPPORTED: {}",
            environment
                .unsupported_reason
                .as_deref()
                .unwrap_or("unknown")
        );
    }

    println!();
    println!("Features");
    for (feature, availability) in &environment.features {
        let mark = if availability.is_available() {
            if availability.is_degraded() {
                "~"
            } else {
                "+"
            }
        } else {
            "-"
        };
        println!("  [{mark}] {}", feature.label());
        if let Some(reason) = availability.reason() {
            for line in wrap(reason, 62) {
                println!("        {line}");
            }
        }
    }

    println!();
    println!("Wallpaper");
    match &detected {
        Some(d) => {
            println!("  Source               {}", paths.display(&d.path));
            println!("  Screen / plugin      {} / {}", d.screen, d.plugin);
            if d.is_slideshow {
                println!("  Note                 slideshow: the accent follows the current frame");
            }
        }
        None => println!("  (not detected; choose an image with --image or in the GUI)"),
    }

    if let Some(report) = &palette_report {
        println!();
        println!("Palette ({})", report.variant.as_str());
        print_swatches(&report.palette);
        println!(
            "  Contrast             worst text {:.2}:1",
            report.worst_text_ratio()
        );
        if args.detailed {
            for check in &report.checks {
                let mark = if check.passes() { "ok" } else { "FAIL" };
                println!(
                    "    {mark:>4} {:>12} on {:<12} {:5.2}:1 (needs {:.1})",
                    check.foreground_href, check.background_href, check.ratio, check.required
                );
            }
        }
    }

    println!();
    println!("Service");
    println!(
        "  Installed            {}",
        if paths.data_dir.join("installed.json").exists() {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "  Automatic updates    {}",
        if settings.general.automatic {
            "on"
        } else {
            "off"
        }
    );
    match state.last_applied_unix {
        Some(ts) => println!("  Last applied         {}", format_relative(ts)),
        None => println!("  Last applied         never"),
    }
    if let Some(accent) = &state.accent {
        println!("  Last accent          {accent}");
    }
    if let Some(error) = &state.last_error {
        println!("  Last error           {error}");
    }
    println!("  Owned changes        {}", ownership.items.len());
    Ok(())
}

fn print_swatches(palette: &ws_palette::Palette) {
    let roles = [
        ("accent", palette.accent),
        ("background", palette.background),
        ("surface", palette.surface),
        ("button", palette.button),
        ("hover", palette.hover),
        ("selection", palette.selection),
        ("text", palette.text),
        ("warning", palette.warning),
    ];
    for (name, color) in roles {
        println!("  {name:<12} {}  {}", color.to_hex_with_alpha(), bar(color));
    }
}

/// A small truecolour bar so the terminal shows the palette. Falls back to
/// nothing when the terminal does not support colour.
fn bar(color: ws_palette::Rgb) -> String {
    if std::env::var_os("NO_COLOR").is_some() {
        return String::new();
    }
    format!(
        "\x1b[48;2;{};{};{}m        \x1b[0m",
        color.r, color.g, color.b
    )
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn format_relative(then: u64) -> String {
    let now = ws_core::snapshot::now_unix();
    let delta = now.saturating_sub(then);
    match delta {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} minutes ago", delta / 60),
        3600..=86399 => format!("{} hours ago", delta / 3600),
        86400..=2_592_000 => format!("{} days ago", delta / 86400),
        _ => format!("on {}", ws_core::snapshot::format_unix_timestamp(then)),
    }
}

fn preview(json: bool, args: PreviewArgs) -> Result<()> {
    let paths = paths()?;
    let backend = backend(&paths)?;
    let settings = Settings::load(&paths.settings_file())
        .map(|(s, _)| s)
        .unwrap_or_default();

    let image = match &args.image {
        Some(path) => path.clone(),
        None => backend
            .detect()
            .context("cannot detect the wallpaper")?
            .map(|d| d.path)
            .context("no wallpaper was detected; pass --image PATH")?,
    };

    let sampled = ws_palette::sample_path(&image)
        .with_context(|| format!("cannot read {}", image.display()))?;
    let derivation = ws_palette::derive(
        &sampled,
        None,
        settings.palette.background_alpha,
        &settings.palette.overrides,
    );
    let report = match settings.general.variant {
        ws_palette::Variant::Dark => derivation.dark.clone(),
        ws_palette::Variant::Light => derivation.light.clone(),
    };
    let preview = backend.preview(&report, &settings);

    if json {
        out(
            true,
            serde_json::json!({
                "image": image,
                "accent": derivation.accent.to_hex(),
                "source": derivation.source,
                "clusters": derivation.clusters,
                "palette": report,
                "would_change": preview.would_change,
                "would_skip": preview.would_skip,
                "color_scheme": preview.color_scheme_sample,
            }),
            || {},
        );
        return Ok(());
    }

    if args.scheme {
        if let Some(sample) = &preview.color_scheme_sample {
            print!("{sample}");
        }
        return Ok(());
    }

    println!("Wallpaper  {}", paths.display(&image));
    println!(
        "Accent     {} ({:?})",
        derivation.accent.to_hex(),
        derivation.source
    );
    println!();
    for cluster in derivation.clusters.iter().take(5) {
        println!(
            "  hue {:>5.0}deg  chroma {:.3}  share {:>5.1}%",
            cluster.hue,
            cluster.chroma,
            cluster.weight * 100.0
        );
    }
    println!();
    println!("Palette ({})", report.variant.as_str());
    print_swatches(&report.palette);
    println!();
    println!(
        "Contrast   worst text {:.2}:1 (needs 4.5:1)",
        report.worst_text_ratio()
    );
    for check in report.failures() {
        println!(
            "  WARNING  {} on {} is {:.2}:1, below {:.1}",
            check.foreground_href, check.background_href, check.ratio, check.required
        );
    }
    println!();
    println!("Applying would:");
    for line in &preview.would_change {
        println!("  - {line}");
    }
    if !preview.would_skip.is_empty() {
        println!("and would skip:");
        for (what, why) in &preview.would_skip {
            println!("  - {what}: {why}");
        }
    }
    Ok(())
}

fn apply(json: bool, args: ApplyArgs) -> Result<()> {
    let paths = paths()?;
    let mut backend = backend(&paths)?;
    if let Some(image) = &args.image {
        if !image.is_file() {
            anyhow::bail!("{} is not a file", image.display());
        }
        backend.manual_image = Some(image.clone());
    }
    let settings = Settings::load(&paths.settings_file())
        .map(|(s, _)| s)
        .context("cannot read settings")?;

    let detected = backend
        .detect()?
        .context("no wallpaper was detected; pass --image PATH")?;
    let sampled = ws_palette::sample_path(&detected.path)
        .with_context(|| format!("cannot read {}", detected.path.display()))?;
    let derivation = ws_palette::derive(
        &sampled,
        None,
        settings.palette.background_alpha,
        &settings.palette.overrides,
    );
    let report = match settings.general.variant {
        ws_palette::Variant::Dark => derivation.dark.clone(),
        ws_palette::Variant::Light => derivation.light.clone(),
    };

    let mut ownership = Ownership::load(&paths.ownership_file()).unwrap_or_default();

    // Snapshot everything apply is about to write.
    let store = SnapshotStore::new(paths.snapshots_dir());
    let mut snapshot = Snapshot::empty(
        store.next_id(ownership.applied_generation),
        ownership.applied_generation,
        "manual-apply",
    );
    let planned = backend.preview(&report, &settings);
    for (what, _) in &planned.would_skip {
        log::debug!("will skip {what}");
    }
    for entry in
        ws_core::snapshot::capture_planned_files(&paths, &settings, &report.palette, report.variant)
    {
        snapshot.entries.insert(entry.id.clone(), entry);
    }

    let result = backend
        .apply(&report, &settings, &mut ownership)
        .context("applying the theme failed")?;

    // The backend's ownership record is the authority on what it wrote; drop
    // predictions it did not use, then fill in the applied state.
    let touched: Vec<String> = ownership
        .items
        .keys()
        .filter(|id| id.starts_with("file:"))
        .cloned()
        .collect();
    snapshot.entries.retain(|id, _| touched.contains(id));
    for entry in snapshot.entries.values_mut() {
        match std::fs::read_to_string(&entry.path) {
            Ok(text) => entry.applied = ws_core::snapshot::CapturedState::Content { text },
            Err(_) if !entry.path.exists() => {
                entry.applied = ws_core::snapshot::CapturedState::Absent
            }
            Err(_) => {}
        }
    }
    let snapshot_id = if snapshot.is_empty() {
        None
    } else {
        store
            .write(&snapshot)
            .context("cannot write the recovery snapshot")?;
        ownership.snapshots.push(snapshot.id.clone());
        Some(snapshot.id.clone())
    };

    // Record the applied state for the watcher and the GUI.
    let state = ws_service::ServiceState {
        wallpaper: Some(detected.path.clone()),
        accent: Some(report.palette.accent.to_hex()),
        variant: report.variant.as_str().to_owned(),
        last_applied_unix: Some(ws_core::snapshot::now_unix()),
        last_error: None,
        consecutive_failures: 0,
        wallpaper_fingerprint: ws_core::WallpaperFingerprint::of(&detected.path).ok(),
    };
    let _ = state.save(&paths.last_applied_file());
    ownership
        .save(&paths.ownership_file())
        .context("cannot save the ownership record")?;

    out(
        json,
        serde_json::json!({
            "snapshot": snapshot_id,
            "accent": report.palette.accent.to_hex(),
            "changed": result.changed,
            "skipped": result.skipped,
            "restart_required": result.restart_required,
        }),
        || {
            println!("Accent     {}", report.palette.accent.to_hex());
            println!("Wallpaper  {}", paths.display(&detected.path));
            if let Some(id) = &snapshot_id {
                println!("Snapshot   {id}");
            }
            println!();
            for line in &result.changed {
                println!("  applied  {line}");
            }
            for (what, why) in &result.skipped {
                println!("  skipped  {what}: {why}");
            }
            for line in &result.restart_required {
                println!();
                println!("Note: {line}");
            }
            if let Some(scheme) = &result.color_scheme {
                println!();
                println!("Active colour scheme: {scheme}");
            }
        },
    );
    Ok(())
}

fn restore(json: bool, args: RestoreArgs) -> Result<()> {
    let paths = paths()?;
    let backend = backend(&paths)?;
    let store = SnapshotStore::new(paths.snapshots_dir());

    let snapshot = match &args.snapshot {
        Some(id) => store
            .load(id)
            .with_context(|| format!("cannot load snapshot {id}"))?,
        None => {
            let latest = store
                .list()
                .into_iter()
                .next()
                .context("there are no snapshots to restore from")?;
            store
                .load(&latest.id)
                .context("cannot load the latest snapshot")?
        }
    };

    let entries = backend.restore(&snapshot, args.force)?;
    let restored = entries
        .iter()
        .filter(|e| {
            matches!(
                e.outcome,
                ws_core::RestoreOutcome::Restored | ws_core::RestoreOutcome::Removed
            )
        })
        .count();
    let conflicts: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e.outcome, ws_core::RestoreOutcome::Conflict))
        .collect();
    let failures: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e.outcome, ws_core::RestoreOutcome::Failed { .. }))
        .collect();

    out(
        json,
        serde_json::json!({
            "snapshot": snapshot.id,
            "restored": restored,
            "conflicts": conflicts.iter().map(|e| e.item.describe()).collect::<Vec<_>>(),
            "failed": failures.iter().map(|e| e.item.describe()).collect::<Vec<_>>(),
        }),
        || {
            println!(
                "Restored {} change(s) from snapshot {}",
                restored, snapshot.id
            );
            if !conflicts.is_empty() {
                println!();
                println!("Left alone because they changed after Wallpaper Style applied them:");
                for entry in &conflicts {
                    println!("  - {}", entry.item.describe());
                }
                println!();
                println!("Use --force to overwrite them with the snapshot's values.");
            }
            for entry in &failures {
                if let ws_core::RestoreOutcome::Failed { message } = &entry.outcome {
                    println!("  failed  {}: {message}", entry.item.describe());
                }
            }
        },
    );
    Ok(())
}

fn install(json: bool, args: InstallArgs) -> Result<()> {
    let paths = paths()?;
    let cli_path = std::env::current_exe().context("cannot determine the executable path")?;
    let installer = PlasmaInstaller::new(paths.clone(), cli_path);
    let mut ownership = Ownership::load(&paths.ownership_file()).unwrap_or_default();
    let report = installer
        .install(&mut ownership)
        .context("installation failed")?;

    let mut enabled = false;
    let mut warnings = report.warnings.clone();
    if !args.no_enable {
        let runner = SystemRunner;
        // `daemon-reload` is needed before enabling a newly written unit.
        let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
        match runner.run(
            "systemctl",
            &["--user", "enable", ws_plasma::install::SERVICE_UNIT],
        ) {
            Ok(_) => {}
            Err(err) => warnings.push(format!(
                "the watcher service could not be enabled ({err}). \
                 Run `systemctl --user enable {}` yourself.",
                ws_plasma::install::SERVICE_UNIT
            )),
        }
        // Always restart rather than `enable --now`. `--now` starts a unit that
        // is stopped and does nothing to one already running, so reinstalling
        // after an upgrade left the old binary running: it kept regenerating the
        // previous version's files on every poll, and the fix the user had just
        // installed silently never took effect.
        match runner.run(
            "systemctl",
            &["--user", "restart", ws_plasma::install::SERVICE_UNIT],
        ) {
            Ok(_) => enabled = true,
            Err(err) => warnings.push(format!(
                "the watcher service is enabled but could not be started ({err}). \
                 Run `systemctl --user restart {}` yourself.",
                ws_plasma::install::SERVICE_UNIT
            )),
        }
    }

    out(
        json,
        serde_json::json!({
            "installed": report.installed,
            "unchanged": report.unchanged,
            "warnings": warnings,
            "service_enabled": enabled,
        }),
        || {
            for line in &report.installed {
                println!("  installed  {line}");
            }
            for line in &report.unchanged {
                println!("  unchanged  {line}");
            }
            for line in &warnings {
                println!("  warning    {line}");
            }
            println!();
            if enabled {
                println!("The background watcher is running.");
            }
            println!("Next: `wallpaper-style preview` then `wallpaper-style apply`.");
        },
    );
    Ok(())
}

fn uninstall(json: bool, args: RestoreArgs) -> Result<()> {
    let paths = paths()?;
    let backend = backend(&paths)?;
    let installer = PlasmaInstaller::new(paths.clone(), PathBuf::new());
    let runner = SystemRunner;

    // Stop the service first so it cannot re-apply while we revert.
    let _ = runner.run(
        "systemctl",
        &[
            "--user",
            "disable",
            "--now",
            ws_plasma::install::SERVICE_UNIT,
        ],
    );

    let mut removed = Vec::new();
    let mut conflicts = Vec::new();
    let mut failed = Vec::new();

    // Revert theme changes from the newest snapshot.
    let store = SnapshotStore::new(paths.snapshots_dir());
    if let Some(latest) = store.list().into_iter().next() {
        if let Ok(snapshot) = store.load(&latest.id) {
            for entry in backend.restore(&snapshot, args.force)? {
                match entry.outcome {
                    ws_core::RestoreOutcome::Restored | ws_core::RestoreOutcome::Removed => {
                        removed.push(entry.item.describe())
                    }
                    ws_core::RestoreOutcome::Conflict => conflicts.push(entry.item.describe()),
                    ws_core::RestoreOutcome::Failed { message } => {
                        failed.push(format!("{}: {message}", entry.item.describe()))
                    }
                    _ => {}
                }
            }
        }
    }

    // Then remove the unit and effect package we installed.
    for entry in installer.uninstall(args.force)? {
        match entry.outcome {
            ws_core::RestoreOutcome::Removed => removed.push(entry.item.describe()),
            ws_core::RestoreOutcome::Failed { message } => {
                failed.push(format!("{}: {message}", entry.item.describe()))
            }
            _ => {}
        }
    }
    let _ = runner.run("systemctl", &["--user", "daemon-reload"]);

    out(
        json,
        serde_json::json!({ "removed": removed, "conflicts": conflicts, "failed": failed }),
        || {
            for line in &removed {
                println!("  removed  {line}");
            }
            if !conflicts.is_empty() {
                println!();
                println!("Kept unchanged (modified after installation):");
                for line in &conflicts {
                    println!("  - {line}");
                }
            }
            for line in &failed {
                println!("  failed   {line}");
            }
            println!();
            println!(
                "Your settings, snapshots and theme files were kept. \
                 Delete {} to remove them too.",
                paths.display(&paths.config_dir)
            );
        },
    );
    Ok(())
}

fn settings_command(json: bool, args: SettingsArgs) -> Result<()> {
    let paths = paths()?;

    if let Some(legacy_path) = &args.import_legacy {
        let (imported, used) = ws_core::settings::import_legacy(legacy_path)
            .with_context(|| format!("cannot import {}", legacy_path.display()))?;
        if !used {
            println!(
                "{} contained no recognisable settings; nothing imported.",
                legacy_path.display()
            );
            return Ok(());
        }
        // Never overwrite an existing modern file without keeping a copy.
        if paths.settings_file().exists() {
            let backup = paths.settings_file().with_extension("toml.bak");
            std::fs::copy(paths.settings_file(), &backup)
                .with_context(|| format!("cannot back up {}", paths.settings_file().display()))?;
            println!(
                "Backed up the previous settings to {}",
                paths.display(&backup)
            );
        }
        imported
            .save(&paths.settings_file())
            .context("cannot write settings")?;
        println!("Imported {}", paths.display(&paths.settings_file()));
        return Ok(());
    }

    let (mut settings, migrated) = Settings::load(&paths.settings_file())?;
    if migrated {
        let backup = paths.settings_file().with_extension("toml.v1.bak");
        let _ = std::fs::copy(paths.settings_file(), &backup);
        println!(
            "Upgraded the settings file to schema version {}",
            ws_core::CURRENT_SCHEMA_VERSION
        );
    }

    for assignment in &args.set {
        apply_setting(&mut settings, assignment)?;
    }
    if !args.set.is_empty() || migrated {
        settings
            .save(&paths.settings_file())
            .context("cannot write settings")?;
    }

    if json {
        let toml = settings.to_toml().unwrap_or_default();
        out(
            true,
            serde_json::json!({ "path": paths.settings_file(), "settings": toml }),
            || {},
        );
        return Ok(());
    }
    if args.show || args.set.is_empty() {
        print!("{}", settings.to_toml().unwrap_or_default());
        println!();
        println!("# {}", paths.display(&paths.settings_file()));
    }
    Ok(())
}

/// Apply one `key=value` assignment.
fn apply_setting(settings: &mut Settings, assignment: &str) -> Result<()> {
    let (key, raw) = assignment
        .split_once('=')
        .with_context(|| format!("expected key=value, got {assignment:?}"))?;
    let key = key.trim();
    let raw = raw.trim();
    let parse_bool = || -> Result<bool> {
        match raw.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok(true),
            "false" | "no" | "off" | "0" => Ok(false),
            other => anyhow::bail!("{other:?} is not a yes/no value"),
        }
    };
    let parse_f32 = || -> Result<f32> {
        raw.parse::<f32>()
            .with_context(|| format!("{raw:?} is not a number"))
    };
    let parse_u64 = || -> Result<u64> {
        raw.parse::<u64>()
            .with_context(|| format!("{raw:?} is not a whole number"))
    };

    match key {
        "general.automatic" => settings.general.automatic = parse_bool()?,
        "general.variant" => {
            settings.general.variant = raw.parse().map_err(|e: String| anyhow::anyhow!(e))?
        }
        "general.autostart" => settings.general.autostart = parse_bool()?,
        "general.tray_icon" => settings.general.tray_icon = parse_bool()?,
        "general.watch_interval_seconds" => settings.general.watch_interval_seconds = parse_u64()?,
        "themes.plasma_color_scheme" => settings.themes.plasma_color_scheme = parse_bool()?,
        "themes.kvantum" => settings.themes.kvantum = parse_bool()?,
        "themes.kvantum_glass_variant" => settings.themes.kvantum_glass_variant = parse_bool()?,
        "themes.kwin_effect" => settings.themes.kwin_effect = parse_bool()?,
        "transparency.enabled" => settings.transparency.enabled = parse_bool()?,
        "transparency.default_opacity" => settings.transparency.default_opacity = parse_f32()?,
        "transparency.skip_fullscreen" => settings.transparency.skip_fullscreen = parse_bool()?,
        "palette.background_alpha" => {
            settings.palette.background_alpha = raw
                .parse::<u8>()
                .with_context(|| format!("{raw:?} is not 0-255"))?;
        }
        "palette.accent" => {
            settings.palette.accent = Some(
                ws_palette::Rgb::from_hex(raw)
                    .with_context(|| format!("{raw:?} is not a #rrggbb colour"))?,
            );
        }
        "palette.manual_image" => settings.palette.manual_image = Some(PathBuf::from(raw)),
        "service.retry_seconds" => settings.service.retry_seconds = parse_u64()?,
        "service.log_level" => {
            if !matches!(raw, "error" | "warn" | "info" | "debug" | "trace") {
                anyhow::bail!("log_level must be one of error, warn, info, debug, trace");
            }
            settings.service.log_level = raw.to_owned();
        }
        "transparency.exclusions" => {
            settings.transparency.exclusions = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
        }
        other => anyhow::bail!(
            "unknown setting {other:?}. Run `wallpaper-style settings --show` to see the schema."
        ),
    }
    Ok(())
}

fn snapshot_command(json: bool, args: SnapshotArgs) -> Result<()> {
    let paths = paths()?;
    let store = SnapshotStore::new(paths.snapshots_dir());
    match args.action {
        SnapshotAction::List => {
            let list = store.list();
            if json {
                out(true, serde_json::json!(list), || {});
                return Ok(());
            }
            if list.is_empty() {
                println!("No snapshots yet. One is taken automatically before each apply.");
                return Ok(());
            }
            println!("{:<24} {:<20} {:>8}  REASON", "ID", "TAKEN", "CHANGES");
            for summary in list {
                println!(
                    "{:<24} {:<20} {:>8}  {}",
                    summary.id,
                    ws_core::snapshot::format_unix_timestamp(summary.created_unix),
                    summary.entries,
                    summary.reason
                );
            }
        }
        SnapshotAction::Show { id } => {
            let snapshot = store
                .load(&id)
                .with_context(|| format!("no snapshot {id:?}"))?;
            if json {
                out(
                    true,
                    serde_json::to_value(&snapshot).unwrap_or_default(),
                    || {},
                );
                return Ok(());
            }
            println!(
                "Snapshot {} ({} entries, {})",
                snapshot.id,
                snapshot.entries.len(),
                snapshot.reason
            );
            for entry in snapshot.entries.values() {
                println!("  {}", entry.path.display());
            }
        }
    }
    Ok(())
}

fn presets_command(json: bool) -> Result<()> {
    if json {
        let list: Vec<_> = presets::PRESETS
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "window_classes": p.window_classes,
                    "default_opacity": p.default_opacity,
                    "kind": p.kind,
                    "note": p.note,
                })
            })
            .collect();
        out(true, serde_json::json!(list), || {});
        return Ok(());
    }
    for preset in presets::PRESETS {
        println!(
            "{}  ({})  default {:.0}%",
            preset.name,
            preset.id,
            preset.default_opacity * 100.0
        );
        println!("  classes: {}", preset.window_classes.join(", "));
        for line in wrap(preset.note, 72) {
            println!("  {line}");
        }
        println!();
    }
    Ok(())
}

fn identify_command(json: bool, window_class: &str) -> Result<()> {
    let identification = presets::identify(window_class);
    if json {
        out(
            true,
            serde_json::to_value(&identification).unwrap_or_default(),
            || {},
        );
        return Ok(());
    }
    println!("Window class  {window_class:?}");
    println!("Words         {}", identification.tokens.join(", "));
    match identification.preset {
        Some(id) => {
            let preset = presets::preset(id).expect("preset id came from the catalogue");
            println!("Preset        {} ({id})", preset.name);
        }
        None => println!("Preset        none"),
    }
    match identification.excluded_reason {
        Some(reason) => println!("Excluded      {}", reason.describe()),
        None => println!("Excluded      no"),
    }
    if let Some(suggested) = identification.suggested_rule_class() {
        println!("Suggested     {suggested}");
        println!();
        println!("Add a rule with:");
        println!("  wallpaper-style settings --set transparency.enabled=true");
        println!("  # then edit transparency.custom in the settings file");
    }
    Ok(())
}

fn service_command(json: bool, args: ServiceArgs) -> Result<()> {
    let paths = paths()?;
    let backend = backend(&paths)?;
    match args.action {
        ServiceAction::Once => {
            let mut watcher = Watcher::new(paths);
            watcher.restore_state();
            match watcher.tick(&backend) {
                Tick::Unchanged => {
                    out(json, serde_json::json!({"result": "unchanged"}), || {
                        println!("No change.");
                    });
                }
                Tick::Applied { wallpaper, accent } => {
                    out(
                        json,
                        serde_json::json!({"result": "applied", "wallpaper": wallpaper, "accent": accent}),
                        || println!("Applied {accent} from {}", wallpaper.display()),
                    );
                }
                Tick::Skipped { reason } => {
                    out(
                        json,
                        serde_json::json!({"result": "skipped", "reason": reason}),
                        || {
                            println!("Skipped: {reason}");
                        },
                    );
                }
                Tick::Failed { reason } => {
                    out(
                        json,
                        serde_json::json!({"result": "failed", "reason": reason}),
                        || {
                            println!("Failed: {reason}");
                        },
                    );
                    return Err(anyhow::anyhow!("{reason}"));
                }
            }
        }
        ServiceAction::Run => {
            let mut watcher = Watcher::new(paths);
            watcher.restore_state();
            println!("Wallpaper Style watcher running. Press Ctrl-C to stop.");
            let _ = std::io::stdout().flush();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = stop.clone();
            let _ = ctrlc_handler(move || flag.store(true, std::sync::atomic::Ordering::SeqCst));
            ws_service::run(&mut watcher, &backend, &RealSleeperShim, || {
                stop.load(std::sync::atomic::Ordering::SeqCst)
            });
        }
    }
    Ok(())
}

/// Adapts the service's `Sleeper` to real time.
struct RealSleeperShim;

impl Sleeper for RealSleeperShim {
    fn sleep(&self, duration: std::time::Duration) {
        // Sleep in short slices so Ctrl-C is noticed promptly.
        let deadline = std::time::Instant::now() + duration;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

/// Install a minimal SIGINT/SIGTERM handler without pulling in a signal crate.
///
/// `nix::sys::signal` is already a dependency of the workspace for this purpose.
fn ctrlc_handler<F: Fn() + Send + Sync + 'static>(f: F) -> Result<()> {
    use std::sync::OnceLock;
    static CALLBACK: OnceLock<Box<dyn Fn() + Send + Sync + 'static>> = OnceLock::new();
    extern "C" fn handler(_: std::os::raw::c_int) {
        if let Some(cb) = CALLBACK.get() {
            cb();
        }
    }
    let boxed: Box<dyn Fn() + Send + Sync + 'static> = Box::new(f);
    if CALLBACK.set(boxed).is_err() {
        return Ok(());
    }
    // SAFETY: `handler` only reads a `OnceLock` and stores to an `AtomicBool`,
    // both of which are async-signal-safe. No allocation happens in the handler.
    for signal in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
    ] {
        unsafe {
            nix::sys::signal::sigaction(
                signal,
                &nix::sys::signal::SigAction::new(
                    nix::sys::signal::SigHandler::Handler(handler),
                    nix::sys::signal::SaFlags::empty(),
                    nix::sys::signal::SigSet::empty(),
                ),
            )
        }
        .context("cannot install the signal handler")?;
    }
    Ok(())
}

fn doctor(json: bool) -> Result<()> {
    let paths = paths()?;
    let runner = SystemRunner;
    let session = ws_plasma::env::SessionInfo::detect(&|key| std::env::var(key).ok(), &runner);
    let toolchain = env::detect_toolchain(&runner);
    let environment = env::environment_report(&session, &toolchain, true, None);

    let checks: Vec<(&str, bool, String)> = vec![
        (
            "KDE Plasma session",
            session.is_plasma(),
            format!("{:?}", session.desktop),
        ),
        (
            "Plasma 6 or newer",
            session.is_plasma6(),
            session
                .desktop_version
                .clone()
                .unwrap_or_else(|| "unknown".into()),
        ),
        (
            "Wayland session",
            session.session_type == "wayland",
            session.session_type.clone(),
        ),
        (
            "qdbus6",
            toolchain.qdbus.is_some(),
            toolchain.qdbus.clone().unwrap_or_default(),
        ),
        (
            "kreadconfig6",
            toolchain.kreadconfig.is_some(),
            toolchain.kreadconfig.clone().unwrap_or_default(),
        ),
        (
            "kwriteconfig6",
            toolchain.kwriteconfig.is_some(),
            toolchain.kwriteconfig.clone().unwrap_or_default(),
        ),
        (
            "plasma-apply-colorscheme",
            toolchain.plasma_apply_colorscheme,
            String::new(),
        ),
        (
            "Kvantum",
            toolchain.has_kvantum,
            toolchain
                .kvantum_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        (
            "systemctl --user",
            runner
                .run("systemctl", &["--user", "is-system-running"])
                .is_ok(),
            String::new(),
        ),
    ];

    if json {
        let payload: Vec<_> = checks
            .iter()
            .map(
                |(name, ok, detail)| serde_json::json!({"check": name, "ok": ok, "detail": detail}),
            )
            .collect();
        out(
            true,
            serde_json::json!({"checks": payload, "environment": environment}),
            || {},
        );
        return Ok(());
    }

    println!("Dependency checks");
    let mut missing = 0;
    for (name, ok, detail) in &checks {
        let mark = if *ok { "ok  " } else { "MISS" };
        if !*ok {
            missing += 1;
        }
        println!("  [{mark}] {name:<26} {detail}");
    }
    println!();
    for feature in Feature::ALL {
        let availability = environment.availability(feature);
        if !availability.is_available() {
            println!(
                "{:?}: {}",
                feature.label(),
                availability.reason().unwrap_or("unavailable")
            );
        }
    }
    if missing > 0 {
        println!();
        println!("Install the missing packages from your distribution, then re-run `wallpaper-style doctor`.");
    }
    println!();
    println!("Settings   {}", paths.display(&paths.settings_file()));
    println!("State      {}", paths.display(&paths.state_dir));
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Tests for the CLI's own logic. Command execution goes through
    //! `ws_plasma::env::Runner`, so these assert on what *would* run.

    use ws_plasma::env::{RecordingRunner, Runner};

    /// The sequence `install` issues when it enables the watcher.
    fn enable_sequence(currently_running: bool) -> Vec<String> {
        let runner = RecordingRunner::with_programs(&["systemctl"]);
        {
            let mut responses = runner.responses.borrow_mut();
            responses.push(("systemctl".into(), Ok(String::new()))); // daemon-reload
            responses.push(("systemctl".into(), Ok(String::new()))); // enable
            if currently_running {
                responses.push(("systemctl".into(), Ok(String::new()))); // restart
            }
        }
        // Mirror super::install's service block.
        let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
        let _ = runner.run(
            "systemctl",
            &["--user", "enable", "wallpaper-style.service"],
        );
        let _ = runner.run(
            "systemctl",
            &["--user", "restart", "wallpaper-style.service"],
        );
        runner
            .calls()
            .into_iter()
            .map(|(_, args)| args.join(" "))
            .collect()
    }

    #[test]
    fn installing_always_restarts_the_watcher() {
        // Regression: `enable --now` leaves an already-running unit alone, so a
        // reinstall kept the previous binary in memory. That binary regenerated
        // the old KWin effect on every poll, silently undoing the upgrade.
        let calls = enable_sequence(true);
        assert!(
            calls.iter().any(|c| c.contains("restart")),
            "a reinstall must restart the service, got {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.contains("--now")),
            "`enable --now` does not restart a running unit, got {calls:?}"
        );
    }

    #[test]
    fn the_unit_is_still_enabled_for_next_login() {
        let calls = enable_sequence(false);
        assert!(calls
            .iter()
            .any(|c| c.contains("enable") && !c.contains("--now")));
    }

    #[test]
    fn daemon_reload_precedes_enable() {
        let calls = enable_sequence(false);
        let reload = calls.iter().position(|c| c.contains("daemon-reload"));
        let enable = calls.iter().position(|c| c.contains("enable"));
        assert!(reload < enable, "daemon-reload must come first: {calls:?}");
    }
}
