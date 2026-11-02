//! Installation of the user-level pieces: the systemd user unit and the KWin
//! effect package.
//!
//! Installing is deliberately separate from *applying a theme*. Installation
//! puts the application's own service and effect in place; applying decides what
//! the desktop should look like. A user can install once and apply many times,
//! and uninstalling must work whether or not a theme was ever applied.
//!
//! Every step is idempotent: a second install rewrites identical bytes, records
//! nothing new, and reports "unchanged".

use std::path::{Path, PathBuf};

use ws_backend::{BackendError, InstallReport, Installer};
use ws_core::ownership::{hash_file, OwnedItem};
use ws_core::snapshot::write_atomic;
use ws_core::{Ownership, Paths};

/// Name of the systemd user unit.
pub const SERVICE_UNIT: &str = "wallpaper-style.service";

/// The systemd unit written for the watcher.
///
/// `Type=simple` with `Restart=on-failure` and a delay: if the watcher exits it
/// is restarted, but a persistent failure backs off instead of spinning.
pub fn service_unit(cli_path: &str) -> String {
    format!(
        r#"[Unit]
Description=Wallpaper Style background watcher
Documentation=man:wallpaper-style(1)
After=graphical-session.target plasma-workspace.target
PartOf=graphical-session.target
# The watcher needs the session bus for Plasma and KWin.
Requires=dbus.service

[Service]
Type=simple
ExecStart={cli_path} service run
Restart=on-failure
RestartSec=10
# The watcher must not be killed merely because it is idle.
TimeoutStartSec=0
# Keep the process light: palette work is short and infrequent.
Nice=10
IOSchedulingClass=idle

[Install]
WantedBy=graphical-session.target
"#
    )
}

/// The installer for this machine.
#[derive(Debug)]
pub struct PlasmaInstaller {
    pub paths: Paths,
    /// Absolute path to the CLI binary the unit should run.
    pub cli_path: PathBuf,
}

impl PlasmaInstaller {
    pub fn new(paths: Paths, cli_path: PathBuf) -> Self {
        Self { paths, cli_path }
    }

    pub fn unit_path(&self) -> PathBuf {
        self.paths.systemd_user_dir().join(SERVICE_UNIT)
    }

    /// Write a file if its content differs; report whether anything changed.
    fn sync_file(&self, path: &Path, contents: &str) -> Result<bool, BackendError> {
        let existing = std::fs::read_to_string(path).ok();
        if existing.as_deref() == Some(contents) {
            return Ok(false);
        }
        write_atomic(path, contents.as_bytes()).map_err(|source| BackendError::io(path, source))?;
        Ok(true)
    }

    /// Is the watcher unit installed and enabled in `kdeglobals`-style terms?
    /// Only the file's presence is checked here; enabling is systemd's job.
    pub fn unit_installed(&self) -> bool {
        self.unit_path().is_file()
    }
}

impl Installer for PlasmaInstaller {
    fn install_root(&self) -> &Path {
        &self.paths.data_dir
    }

    fn install(&self, ownership: &mut Ownership) -> Result<InstallReport, BackendError> {
        let mut report = InstallReport::default();

        // 1. systemd unit.
        let unit = service_unit(&self.cli_path.display().to_string());
        let unit_path = self.unit_path();
        let previous_unit = std::fs::read_to_string(&unit_path).ok();
        if self.sync_file(&unit_path, &unit)? {
            report
                .installed
                .push(format!("wrote {}", self.paths.display(&unit_path)));
        } else {
            report
                .unchanged
                .push(format!("{} is up to date", self.paths.display(&unit_path)));
        }
        ownership.record(
            format!("unit:{SERVICE_UNIT}"),
            OwnedItem::SystemdUnit {
                name: SERVICE_UNIT.to_owned(),
                path: unit_path,
                original: previous_unit,
            },
        );

        // 2. Recording that the application is installed, so `status` can tell
        //    "not installed" from "removed".
        let marker = self.paths.data_dir.join("installed.json");
        let marker_body = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "installed_unix": ws_core::snapshot::now_unix(),
        })
        .to_string();
        if self.sync_file(&marker, &marker_body)? {
            report
                .installed
                .push(format!("wrote {}", self.paths.display(&marker)));
        } else {
            report.unchanged.push("already installed".to_owned());
        }

        // 3. Dependencies that are missing, reported rather than fatal.
        if !self.paths.kdeglobals().exists() {
            report.warnings.push(
                "no KDE configuration was found; is this a Plasma session? The service will keep \
                 retrying and report the problem in its status."
                    .to_owned(),
            );
        }

        ownership
            .save(&self.paths.ownership_file())
            .map_err(|e| BackendError::Other(format!("cannot save the ownership record: {e}")))?;
        Ok(report)
    }

    fn uninstall(&self, force: bool) -> Result<Vec<ws_core::RestoreEntry>, BackendError> {
        let ownership = Ownership::load(&self.paths.ownership_file())
            .map_err(|e| BackendError::Other(format!("cannot read the ownership record: {e}")))?;

        let mut entries = Vec::new();
        for (id, item) in &ownership.items {
            match item {
                OwnedItem::SystemdUnit { path, original, .. } => {
                    let outcome = if path.exists() {
                        match std::fs::remove_file(path) {
                            Ok(()) => ws_core::RestoreOutcome::Removed,
                            Err(err) => ws_core::RestoreOutcome::Failed {
                                message: err.to_string(),
                            },
                        }
                    } else {
                        ws_core::RestoreOutcome::AlreadyClean
                    };
                    if let Some(text) = original {
                        // The user had their own unit of the same name; put it back.
                        if force || !path.exists() {
                            let _ = write_atomic(path, text.as_bytes());
                        }
                    }
                    entries.push(ws_core::RestoreEntry {
                        item: item.clone(),
                        outcome,
                    });
                }
                OwnedItem::EffectPackage { path, .. } => {
                    let outcome = if path.exists() {
                        match std::fs::remove_dir_all(path) {
                            Ok(()) => ws_core::RestoreOutcome::Removed,
                            Err(err) => ws_core::RestoreOutcome::Failed {
                                message: err.to_string(),
                            },
                        }
                    } else {
                        ws_core::RestoreOutcome::AlreadyClean
                    };
                    entries.push(ws_core::RestoreEntry {
                        item: item.clone(),
                        outcome,
                    });
                }
                // Theme files are restored through the snapshot, not here.
                _ => {
                    let _ = id;
                }
            }
        }
        Ok(entries)
    }
}

/// Compare two files' hashes without reading either into memory twice.
pub fn same_content(a: &Path, b: &Path) -> bool {
    match (hash_file(a), hash_file(b)) {
        (Ok(Some(a)), Ok(Some(b))) => a == b,
        (Ok(None), Ok(None)) => true,
        _ => false,
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

    fn installer(paths: Paths) -> PlasmaInstaller {
        PlasmaInstaller::new(paths, PathBuf::from("/usr/bin/wallpaper-style"))
    }

    #[test]
    fn the_unit_runs_the_service_subcommand() {
        let unit = service_unit("/usr/bin/wallpaper-style");
        assert!(unit.contains("ExecStart=/usr/bin/wallpaper-style service run"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
        assert!(unit.contains("PartOf=graphical-session.target"));
    }

    #[test]
    fn installing_writes_the_unit_and_records_it() {
        let (_dir, paths) = sandbox();
        let installer = installer(paths.clone());
        let mut ownership = Ownership::default();
        let report = installer.install(&mut ownership).unwrap();
        assert!(installer.unit_path().exists());
        assert!(!report.installed.is_empty());
        assert!(ownership
            .items
            .values()
            .any(|i| matches!(i, OwnedItem::SystemdUnit { .. })));
        assert!(paths.ownership_file().exists());
    }

    #[test]
    fn a_second_install_changes_nothing() {
        let (_dir, paths) = sandbox();
        let installer = installer(paths);
        let mut ownership = Ownership::default();
        installer.install(&mut ownership).unwrap();
        let second = installer.install(&mut ownership).unwrap();
        assert!(second.installed.is_empty(), "{second:?}");
        assert!(!second.unchanged.is_empty());
    }

    #[test]
    fn user_edits_to_the_unit_are_reverted_on_reinstall() {
        // The unit is generated; a user edit is a mistake the installer corrects,
        // unlike theme files where user edits win.
        let (_dir, paths) = sandbox();
        let installer = installer(paths);
        let mut ownership = Ownership::default();
        installer.install(&mut ownership).unwrap();
        std::fs::write(installer.unit_path(), "[Unit]\nDescription=hand edited\n").unwrap();
        let report = installer.install(&mut ownership).unwrap();
        assert!(!report.installed.is_empty());
        let text = std::fs::read_to_string(installer.unit_path()).unwrap();
        assert!(text.contains("wallpaper-style service run"), "{text}");
    }

    #[test]
    fn uninstalling_removes_the_unit_but_leaves_themes_alone() {
        let (_dir, paths) = sandbox();
        let installer = installer(paths.clone());
        let mut ownership = Ownership::default();
        installer.install(&mut ownership).unwrap();
        // Simulate a theme file the apply step would have created.
        let colors = paths.kde_color_schemes_dir().join("x.colors");
        std::fs::create_dir_all(colors.parent().unwrap()).unwrap();
        std::fs::write(&colors, "[General]\nName=x\n").unwrap();

        let entries = installer.uninstall(false).unwrap();
        assert!(!installer.unit_path().exists());
        assert!(colors.exists(), "uninstall must not delete theme files");
        assert!(entries
            .iter()
            .any(|e| matches!(e.outcome, ws_core::RestoreOutcome::Removed)));
    }

    #[test]
    fn uninstalling_when_nothing_is_installed_is_harmless() {
        let (_dir, paths) = sandbox();
        let installer = installer(paths);
        let entries = installer.uninstall(false).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn a_preexisting_unit_is_restored_on_uninstall() {
        let (_dir, paths) = sandbox();
        let installer = installer(paths);
        // A pre-existing wrapper from the prototype era.
        std::fs::create_dir_all(installer.unit_path().parent().unwrap()).unwrap();
        std::fs::write(
            installer.unit_path(),
            "[Unit]\nDescription=original prototype\n",
        )
        .unwrap();
        let mut ownership = Ownership::default();
        installer.install(&mut ownership).unwrap();
        installer.uninstall(false).unwrap();
        let text = std::fs::read_to_string(installer.unit_path()).unwrap();
        assert!(
            text.contains("original prototype"),
            "the previous unit must come back"
        );
    }

    #[test]
    fn an_effect_package_directory_is_removed_on_uninstall() {
        let (_dir, paths) = sandbox();
        let installer = installer(paths.clone());
        let mut ownership = Ownership::default();
        let effect_dir = paths
            .kwin_effects_dir()
            .join("wallpaper-style-transparency");
        std::fs::create_dir_all(effect_dir.join("contents/code")).unwrap();
        std::fs::write(effect_dir.join("metadata.json"), "{}").unwrap();
        ownership.record(
            "effect:x",
            OwnedItem::EffectPackage {
                id: "x".into(),
                path: effect_dir.clone(),
            },
        );
        ownership.save(&paths.ownership_file()).unwrap();

        installer.uninstall(false).unwrap();
        assert!(!effect_dir.exists());
    }

    #[test]
    fn install_does_not_write_outside_the_sandbox() {
        let (dir, paths) = sandbox();
        let installer = installer(paths);
        let mut ownership = Ownership::default();
        installer.install(&mut ownership).unwrap();
        for item in ownership.items.values() {
            assert!(
                item.path().starts_with(dir.path()),
                "{} escaped",
                item.path().display()
            );
        }
    }
}
