//! Environment detection and external command execution.
//!
//! Everything this backend knows about the outside world enters here, so tests
//! can substitute a fake command runner and a fake environment.

use std::path::{Path, PathBuf};
use std::process::Command;

use ws_backend::{Availability, EnvironmentReport, Feature};

use crate::kwin_effect::EFFECT_ID;

/// How an external command is executed. The production implementation shells
/// out; tests use [`CommandRunner::recording`] so nothing is ever run.
pub trait Runner: std::fmt::Debug {
    /// Run a command, returning its combined output on success.
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;

    /// Whether a program exists on `PATH`.
    fn has(&self, program: &str) -> bool;
}

/// Runs real commands. Read-only commands only in normal operation; the
/// mutating ones go through [`crate::PlasmaBackend`] explicitly.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(if stderr.is_empty() {
                format!("{program} exited with {}", output.status)
            } else {
                format!("{program}: {stderr}")
            })
        }
    }

    fn has(&self, program: &str) -> bool {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v -- {}", shell_quote(program)))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Quote a value for a POSIX shell. Only used for the fixed program names this
/// module passes, never for user input.
fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// A runner that records what would have run. Used by tests.
#[derive(Debug, Default)]
pub struct RecordingRunner {
    pub calls: std::cell::RefCell<Vec<(String, Vec<String>)>>,
    /// Programs that should be reported as present.
    pub present: Vec<String>,
    /// Program → output to return.
    pub responses: std::cell::RefCell<Vec<(String, Result<String, String>)>>,
}

impl RecordingRunner {
    pub fn with_programs(programs: &[&str]) -> Self {
        Self {
            calls: Default::default(),
            present: programs.iter().map(|p| (*p).to_owned()).collect(),
            responses: Default::default(),
        }
    }

    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.borrow().clone()
    }

    pub fn called(&self, program: &str) -> bool {
        self.calls.borrow().iter().any(|(p, _)| p == program)
    }
}

impl Runner for RecordingRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        self.calls.borrow_mut().push((
            program.to_owned(),
            args.iter().map(|a| (*a).to_owned()).collect(),
        ));
        let mut responses = self.responses.borrow_mut();
        match responses.iter().position(|(p, _)| p == program) {
            Some(index) => responses.remove(index).1,
            None => Err(format!("{program}: no recorded response")),
        }
    }

    fn has(&self, program: &str) -> bool {
        self.present.iter().any(|p| p == program)
    }
}

/// Facts about the running session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub desktop: String,
    pub session_type: String,
    pub desktop_version: Option<String>,
    pub compositor: Option<String>,
}

impl SessionInfo {
    pub fn detect(env: &dyn Fn(&str) -> Option<String>, runner: &dyn Runner) -> Self {
        let desktop = env("XDG_CURRENT_DESKTOP")
            .or_else(|| env("XDG_SESSION_DESKTOP"))
            .unwrap_or_default();
        let session_type = env("XDG_SESSION_TYPE")
            .or_else(|| env("WAYLAND_DISPLAY").map(|_| "wayland".to_owned()))
            .unwrap_or_else(|| "unknown".to_owned());

        // `plasmashell --version` aborts on some builds, so prefer KWin's
        // version, which is what actually matters for the effect.
        let desktop_version = runner
            .run("kwin_wayland", &["--version"])
            .or_else(|_| runner.run("kwin_x11", &["--version"]))
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());

        let compositor = if session_type == "wayland" && runner.has("kwin_wayland") {
            Some("kwin_wayland".to_owned())
        } else if session_type == "x11" && runner.has("kwin_x11") {
            Some("kwin_x11".to_owned())
        } else {
            None
        };

        Self {
            desktop,
            session_type,
            desktop_version,
            compositor,
        }
    }

    /// True when this is a KDE Plasma session.
    pub fn is_plasma(&self) -> bool {
        let upper = self.desktop.to_ascii_uppercase();
        upper.contains("KDE") || upper.contains("PLASMA")
    }

    /// True when this is Plasma 6 or newer, judged from the KWin version.
    pub fn is_plasma6(&self) -> bool {
        match self.desktop_version.as_deref().and_then(parse_kwin_version) {
            Some((major, _minor)) => major >= 6,
            // Without a version, trust a KDE desktop rather than refusing to
            // work: Plasma 6 is the current release.
            None => self.is_plasma(),
        }
    }
}

/// Parse `kwin 6.8.1` or `kwin_wayland 6.8.1` into `(major, minor)`.
fn parse_kwin_version(text: &str) -> Option<(u32, u32)> {
    let version = text
        .split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// What is installed on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Toolchain {
    pub qdbus: Option<String>,
    pub kwriteconfig: Option<String>,
    pub kreadconfig: Option<String>,
    pub plasma_apply_colorscheme: bool,
    pub kpackagetool: Option<String>,
    /// Root directories where Kvantum themes may live, user first.
    pub kvantum_roots: Vec<PathBuf>,
    pub has_kvantum: bool,
    pub compositing_active: bool,
}

/// Locate the tools, preferring the KF6 names.
pub fn detect_toolchain(runner: &dyn Runner) -> Toolchain {
    let pick = |candidates: &[&str]| -> Option<String> {
        candidates
            .iter()
            .find(|c| runner.has(c))
            .map(|c| (*c).to_owned())
    };
    let kvantum_roots: Vec<PathBuf> = ["/usr/share/Kvantum", "/usr/local/share/Kvantum"]
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .collect();
    let has_kvantum =
        !kvantum_roots.is_empty() || runner.has("kvantummanager") || runner.has("kvantum-settings");

    Toolchain {
        qdbus: pick(&["qdbus6", "qdbus"]),
        kwriteconfig: pick(&["kwriteconfig6", "kwriteconfig5"]),
        kreadconfig: pick(&["kreadconfig6", "kreadconfig5"]),
        plasma_apply_colorscheme: runner.has("plasma-apply-colorscheme"),
        kpackagetool: pick(&["kpackagetool6", "kpackagetool5"]),
        kvantum_roots,
        has_kvantum,
        compositing_active: false,
    }
}

/// Build the environment report the GUI displays.
///
/// Every `Unavailable`/`Degraded` here is a limitation the product states
/// honestly rather than discovering at apply time.
pub fn environment_report(
    session: &SessionInfo,
    toolchain: &Toolchain,
    compositing_active: bool,
    compositing_reason: Option<&str>,
) -> EnvironmentReport {
    let mut features = Vec::with_capacity(Feature::ALL.len());

    if !session.is_plasma() {
        let reason = format!(
            "This session reports desktop {:?}; Wallpaper Style currently supports KDE Plasma 6 only.",
            if session.desktop.is_empty() { "unknown" } else { &session.desktop }
        );
        for feature in Feature::ALL {
            features.push((feature, Availability::unavailable(reason.clone())));
        }
        return EnvironmentReport {
            desktop: session.desktop.clone(),
            session_type: session.session_type.clone(),
            desktop_version: session.desktop_version.clone(),
            compositor: session.compositor.clone(),
            supported: false,
            unsupported_reason: Some(reason),
            features,
        };
    }

    let wallpaper = if toolchain.qdbus.is_some() {
        Availability::available()
    } else if toolchain.kreadconfig.is_some() {
        Availability::degraded(
            "qdbus6 is missing, so the live wallpaper is read from Plasma's configuration file. \
             Changes made in System Settings are picked up on the next poll rather than instantly.",
        )
    } else {
        Availability::unavailable(
            "Neither qdbus6 nor kreadconfig6 is installed, so the wallpaper cannot be read. \
             Install plasma-workspace and kconfig, or choose an image manually.",
        )
    };

    let color_scheme = if toolchain.plasma_apply_colorscheme {
        Availability::available()
    } else {
        Availability::unavailable(
            "plasma-apply-colorscheme is missing (package plasma-workspace). The colour scheme \
             file can still be generated and selected in System Settings.",
        )
    };

    let kvantum = if !toolchain.has_kvantum {
        Availability::unavailable(
            "Kvantum is not installed, so Qt applications keep their current theme. Plasma \
             colours and window transparency are unaffected.",
        )
    } else if toolchain.kvantum_roots.is_empty() {
        Availability::degraded(
            "Kvantum is installed but no theme directory was found, so there is no base theme to \
             recolour. Select a theme in Kvantum Manager first.",
        )
    } else {
        Availability::available()
    };

    let window_opacity = if session.session_type == "wayland" {
        if compositing_active {
            Availability::available()
        } else {
            Availability::unavailable(
                "The compositor is not running, so window opacity cannot be applied. Enable \
                 compositing in System Settings → Display and Monitor → Compositor.",
            )
        }
    } else if session.session_type == "x11" {
        Availability::degraded(
            "Under X11 the effect behaves the same, but only Plasma's own compositor is \
             supported; other compositors are not tested.",
        )
    } else {
        Availability::unavailable(format!(
            "The session type {:?} was not recognised; only KDE Plasma 6 on Wayland is supported.",
            session.session_type
        ))
    };

    let background_transparency = if !toolchain.has_kvantum {
        Availability::unavailable(
            "Background-only transparency is provided by the Kvantum theme, which is not installed.",
        )
    } else {
        Availability::degraded(
            "Applies to Qt applications that use Kvantum. Electron apps, browsers and apps with \
             custom-drawn surfaces ignore it and get whole-window opacity instead.",
        )
    };

    // Wallpaper changes are detected by polling the configuration and the
    // file's content fingerprint. That works regardless of the compositor, so
    // it is always available; there is no inotify-based instant path yet.
    let file_watching = Availability::available();

    features.push((Feature::WallpaperDetection, wallpaper));
    features.push((Feature::ColorScheme, color_scheme));
    features.push((Feature::KvantumTheme, kvantum));
    features.push((Feature::WindowOpacity, window_opacity));
    features.push((Feature::BackgroundTransparency, background_transparency));
    features.push((Feature::FileWatching, file_watching));
    features.push((
        Feature::TrayIcon,
        Availability::degraded(
            "The tray icon is optional and off by default; it only appears while the control \
             centre is running.",
        ),
    ));

    if let Some(reason) = compositing_reason {
        log::debug!("compositor reported: {reason}");
    }

    EnvironmentReport {
        desktop: session.desktop.clone(),
        session_type: session.session_type.clone(),
        desktop_version: session.desktop_version.clone(),
        compositor: session.compositor.clone(),
        supported: true,
        unsupported_reason: None,
        features,
    }
}

/// The directory the effect is installed to.
pub fn effect_dir(data_home: &Path) -> PathBuf {
    data_home.join("kwin/effects").join(EFFECT_ID)
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

    fn plasma_session() -> SessionInfo {
        SessionInfo {
            desktop: "KDE".into(),
            session_type: "wayland".into(),
            desktop_version: Some("kwin 6.8.1".into()),
            compositor: Some("kwin_wayland".into()),
        }
    }

    fn full_toolchain() -> Toolchain {
        Toolchain {
            qdbus: Some("qdbus6".into()),
            kwriteconfig: Some("kwriteconfig6".into()),
            kreadconfig: Some("kreadconfig6".into()),
            plasma_apply_colorscheme: true,
            kpackagetool: Some("kpackagetool6".into()),
            kvantum_roots: vec![PathBuf::from("/usr/share/Kvantum")],
            has_kvantum: true,
            compositing_active: true,
        }
    }

    #[test]
    fn kwin_versions_parse() {
        assert_eq!(parse_kwin_version("kwin 6.8.1"), Some((6, 8)));
        assert_eq!(parse_kwin_version("kwin_wayland 6.0"), Some((6, 0)));
        assert_eq!(parse_kwin_version("5.27.11"), Some((5, 27)));
        assert_eq!(parse_kwin_version(""), None);
        assert_eq!(parse_kwin_version("no version here"), None);
    }

    #[test]
    fn plasma_six_is_recognised() {
        assert!(plasma_session().is_plasma());
        assert!(plasma_session().is_plasma6());
    }

    #[test]
    fn plasma_five_is_not_treated_as_six() {
        let mut session = plasma_session();
        session.desktop_version = Some("kwin 5.27.11".into());
        assert!(session.is_plasma());
        assert!(!session.is_plasma6());
    }

    #[test]
    fn a_non_kde_desktop_is_reported_unsupported() {
        let session = SessionInfo {
            desktop: "GNOME".into(),
            session_type: "wayland".into(),
            desktop_version: None,
            compositor: None,
        };
        let report = environment_report(&session, &full_toolchain(), true, None);
        assert!(!report.supported);
        assert!(report
            .unsupported_reason
            .as_deref()
            .unwrap()
            .contains("GNOME"));
        for feature in Feature::ALL {
            assert!(
                !report.availability(feature).is_available(),
                "{feature:?} should be off"
            );
        }
    }

    #[test]
    fn a_fully_equipped_plasma_session_has_every_feature() {
        let report = environment_report(&plasma_session(), &full_toolchain(), true, None);
        assert!(report.supported);
        assert!(report
            .availability(Feature::WallpaperDetection)
            .is_available());
        assert!(report.availability(Feature::ColorScheme).is_available());
        assert!(report.availability(Feature::KvantumTheme).is_available());
        assert!(report.availability(Feature::WindowOpacity).is_available());
    }

    #[test]
    fn missing_kvantum_is_explained_not_hidden() {
        let mut tools = full_toolchain();
        tools.has_kvantum = false;
        tools.kvantum_roots.clear();
        let report = environment_report(&plasma_session(), &tools, true, None);
        let availability = report.availability(Feature::KvantumTheme);
        assert!(!availability.is_available());
        assert!(
            availability.reason().unwrap().contains("Kvantum"),
            "{availability:?}"
        );
        // The rest must still work.
        assert!(report.availability(Feature::ColorScheme).is_available());
        assert!(report.availability(Feature::WindowOpacity).is_available());
    }

    #[test]
    fn compositing_off_disables_window_opacity_with_a_reason() {
        let report = environment_report(&plasma_session(), &full_toolchain(), false, None);
        let availability = report.availability(Feature::WindowOpacity);
        assert!(!availability.is_available());
        assert!(
            availability.reason().unwrap().contains("compositor"),
            "{availability:?}"
        );
    }

    #[test]
    fn missing_qdbus_degrades_detection_rather_than_disabling_it() {
        let mut tools = full_toolchain();
        tools.qdbus = None;
        let report = environment_report(&plasma_session(), &tools, true, None);
        let availability = report.availability(Feature::WallpaperDetection);
        assert!(availability.is_available());
        assert!(availability.is_degraded());
    }

    #[test]
    fn missing_kreadconfig_and_qdbus_disables_detection() {
        let mut tools = full_toolchain();
        tools.qdbus = None;
        tools.kreadconfig = None;
        let report = environment_report(&plasma_session(), &tools, true, None);
        assert!(!report
            .availability(Feature::WallpaperDetection)
            .is_available());
    }

    #[test]
    fn the_report_never_claims_background_transparency_works_everywhere() {
        let report = environment_report(&plasma_session(), &full_toolchain(), true, None);
        let availability = report.availability(Feature::BackgroundTransparency);
        assert!(
            availability.is_degraded(),
            "background-only transparency has real limits and must not be advertised as universal"
        );
    }

    #[test]
    fn session_detection_reads_the_environment() {
        let env = |key: &str| match key {
            "XDG_CURRENT_DESKTOP" => Some("KDE".to_owned()),
            "XDG_SESSION_TYPE" => Some("wayland".to_owned()),
            _ => None,
        };
        let runner = RecordingRunner::with_programs(&["kwin_wayland"]);
        runner
            .responses
            .borrow_mut()
            .push(("kwin_wayland".into(), Ok("kwin 6.8.1".into())));
        let info = SessionInfo::detect(&env, &runner);
        assert_eq!(info.desktop, "KDE");
        assert_eq!(info.session_type, "wayland");
        assert_eq!(info.desktop_version.as_deref(), Some("kwin 6.8.1"));
        assert_eq!(info.compositor.as_deref(), Some("kwin_wayland"));
    }

    #[test]
    fn session_detection_survives_a_missing_version_command() {
        let env = |key: &str| match key {
            "XDG_SESSION_TYPE" => Some("x11".to_owned()),
            "XDG_CURRENT_DESKTOP" => Some("KDE".to_owned()),
            _ => None,
        };
        let runner = RecordingRunner::with_programs(&[]);
        let info = SessionInfo::detect(&env, &runner);
        assert_eq!(info.desktop_version, None);
        assert_eq!(info.compositor, None);
        // Still treated as Plasma 6 so the product does not refuse to run.
        assert!(info.is_plasma6());
    }

    #[test]
    fn toolchain_detection_prefers_kf6_names() {
        let runner = RecordingRunner::with_programs(&[
            "qdbus6",
            "kwriteconfig6",
            "kreadconfig6",
            "plasma-apply-colorscheme",
        ]);
        let tools = detect_toolchain(&runner);
        assert_eq!(tools.qdbus.as_deref(), Some("qdbus6"));
        assert_eq!(tools.kwriteconfig.as_deref(), Some("kwriteconfig6"));
        assert!(tools.plasma_apply_colorscheme);
    }

    #[test]
    fn toolchain_detection_reports_absence_rather_than_failing() {
        let runner = RecordingRunner::with_programs(&[]);
        let tools = detect_toolchain(&runner);
        assert_eq!(tools.qdbus, None);
        assert!(!tools.plasma_apply_colorscheme);
    }

    #[test]
    fn the_effect_lives_under_xdg_data_home() {
        assert_eq!(
            effect_dir(Path::new("/home/x/.local/share")),
            PathBuf::from("/home/x/.local/share/kwin/effects/wallpaper-style-transparency")
        );
    }

    #[test]
    fn recording_runner_never_executes_anything() {
        let runner = RecordingRunner::with_programs(&["qdbus6"]);
        let _ = runner.run("qdbus6", &["a", "b"]);
        assert!(runner.called("qdbus6"));
        assert_eq!(runner.calls()[0].1, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn shell_quoting_handles_simple_names_without_quotes() {
        assert_eq!(shell_quote("qdbus6"), "qdbus6");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
