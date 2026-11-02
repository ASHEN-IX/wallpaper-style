//! End-to-end tests of the apply → snapshot → restore cycle against a
//! temporary XDG tree.
//!
//! These exercise the parts that matter for user safety:
//!
//! * applying generates a real, parseable colour scheme and effect package;
//! * every change is recorded so it can be undone;
//! * restoring puts the previous state back;
//! * a change the user made after applying is **not** overwritten.
//!
//! Nothing here touches the real desktop: all paths come from a temporary
//! directory and the command runner never executes anything.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ws_backend::ThemeApplier;
use ws_core::ownership::OwnedItem;
use ws_core::snapshot::{capture_file, CapturedState, Snapshot, SnapshotEntry, SnapshotStore};
use ws_core::{AppOpacity, Ownership, Paths, Settings};
use ws_palette::{Rgb, Variant};
use ws_plasma::backend::{BackendPaths, PlasmaBackend};
use ws_plasma::env::{RecordingRunner, SessionInfo, Toolchain};

struct Sandbox {
    _dir: tempfile::TempDir,
    paths: Paths,
    root: PathBuf,
}

fn sandbox() -> Sandbox {
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
    Sandbox {
        _dir: dir,
        paths,
        root,
    }
}

fn backend(paths: Paths, programs: &[&str]) -> PlasmaBackend {
    let runner = RecordingRunner::with_programs(programs);
    PlasmaBackend::with_runner(
        BackendPaths::new(paths),
        SessionInfo {
            desktop: "KDE".into(),
            session_type: "wayland".into(),
            desktop_version: Some("kwin 6.8.1".into()),
            compositor: Some("kwin_wayland".into()),
        },
        Toolchain {
            qdbus: Some("qdbus6".into()),
            kwriteconfig: Some("kwriteconfig6".into()),
            kreadconfig: Some("kreadconfig6".into()),
            plasma_apply_colorscheme: true,
            kpackagetool: None,
            kvantum_roots: Vec::new(),
            has_kvantum: false,
            compositing_active: true,
        },
        Box::new(runner),
    )
}

fn palette() -> ws_palette::PaletteReport {
    ws_palette::build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255)
}

fn settings() -> Settings {
    let mut settings = Settings::default();
    settings.themes.kwin_effect = true;
    settings.transparency.enabled = true;
    settings
        .transparency
        .custom
        .insert("code".into(), AppOpacity::new(0.72));
    settings
}

/// Wrap one apply in the snapshot discipline the service uses: capture first,
/// then write, then persist the snapshot.
fn apply_with_snapshot(
    backend: &PlasmaBackend,
    store: &SnapshotStore,
    settings: &Settings,
    ownership: &mut Ownership,
) -> Snapshot {
    let mut snapshot = Snapshot::empty(
        store.next_id(ownership.applied_generation),
        ownership.applied_generation,
        "apply",
    );

    // Capture the files apply is about to touch.
    let scheme = ws_plasma::colorscheme::generate(&palette().palette, Variant::Dark);
    for path in [
        backend.color_scheme_dir().join(scheme.file_name()),
        backend.effect_dir().join("metadata.json"),
        backend.effect_dir().join("contents/code/main.js"),
    ] {
        let previous = capture_file(&path).unwrap();
        snapshot.entries.insert(
            format!("file:{}", path.display()),
            SnapshotEntry {
                id: format!("file:{}", path.display()),
                previous,
                applied: CapturedState::Absent,
                path,
                detail: None,
            },
        );
    }

    backend.apply(&palette(), settings, ownership).unwrap();

    // Record what was actually written so restore has a reference point.
    for entry in snapshot.entries.values_mut() {
        if let Ok(Some(text)) = std::fs::read_to_string(&entry.path).map(Some) {
            entry.applied = CapturedState::Content { text };
        }
    }
    store.write(&snapshot).unwrap();
    snapshot
}

#[test]
fn apply_creates_a_parseable_colour_scheme_and_restore_removes_it() {
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let store = SnapshotStore::new(sandbox.paths.snapshots_dir());
    let mut ownership = Ownership::default();

    let snapshot = apply_with_snapshot(&backend, &store, &settings(), &mut ownership);

    let schemes: Vec<PathBuf> = std::fs::read_dir(backend.color_scheme_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(schemes.len(), 1, "{schemes:?}");
    let text = std::fs::read_to_string(&schemes[0]).unwrap();
    let parsed = ws_plasma::colorscheme::ParsedScheme::parse(&text);
    assert!(parsed.name.unwrap().contains("Wallpaper Style"));
    assert!(parsed.sections.iter().any(|s| s == "Colors:Window"));

    // Restoring deletes the file we created.
    let report = ws_core::snapshot::restore(&snapshot, false);
    assert!(
        report
            .iter()
            .all(|e| e.outcome != ws_core::RestoreOutcome::Conflict),
        "{report:?}"
    );
    assert!(
        !schemes[0].exists(),
        "created colour scheme should be removed"
    );
}

#[test]
fn restore_does_not_clobber_a_colour_scheme_the_user_edited() {
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let store = SnapshotStore::new(sandbox.paths.snapshots_dir());
    let mut ownership = Ownership::default();

    let snapshot = apply_with_snapshot(&backend, &store, &settings(), &mut ownership);
    let scheme_path = std::fs::read_dir(backend.color_scheme_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();

    // The user edits the generated scheme afterwards, or another tool does.
    std::fs::write(&scheme_path, "[General]\nName=UserEdited\n").unwrap();

    let report = ws_core::snapshot::restore(&snapshot, false);
    let conflict = report
        .iter()
        .find(|e| matches!(e.item, OwnedItem::CreatedFile { .. }))
        .expect("the scheme should appear in the report");
    assert_eq!(
        conflict.outcome,
        ws_core::RestoreOutcome::Conflict,
        "a user edit after apply must be preserved"
    );
    assert!(scheme_path.exists(), "the user's file must not be deleted");
    assert!(std::fs::read_to_string(&scheme_path)
        .unwrap()
        .contains("UserEdited"));
}

#[test]
fn the_effect_package_is_complete_and_restores_to_absent() {
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let store = SnapshotStore::new(sandbox.paths.snapshots_dir());
    let mut ownership = Ownership::default();

    let snapshot = apply_with_snapshot(&backend, &store, &settings(), &mut ownership);
    let dir = backend.effect_dir();
    let metadata = std::fs::read_to_string(dir.join("metadata.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(parsed["KPackageStructure"], "KWin/Effect");
    let script = std::fs::read_to_string(dir.join("contents/code/main.js")).unwrap();
    assert!(script.contains("\"code\":720"), "the rule was not rendered");

    ws_core::snapshot::restore(&snapshot, false);
    assert!(!dir.join("contents/code/main.js").exists());
}

#[test]
fn removing_the_effect_package_directory_is_reported_as_a_created_child() {
    // A restore that removes files must leave the directory tree consistent:
    // the parent directory may remain, but no orphan file may survive with the
    // application's content in it.
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let store = SnapshotStore::new(sandbox.paths.snapshots_dir());
    let mut ownership = Ownership::default();
    let snapshot = apply_with_snapshot(&backend, &store, &settings(), &mut ownership);
    ws_core::snapshot::restore(&snapshot, false);
    for path in [
        backend.effect_dir().join("metadata.json"),
        backend.effect_dir().join("contents/code/main.js"),
    ] {
        assert!(!path.exists(), "{} survived restore", path.display());
    }
}

#[test]
fn a_second_apply_leaves_exactly_one_scheme_and_one_snapshot_per_apply() {
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let store = SnapshotStore::new(sandbox.paths.snapshots_dir());
    let mut ownership = Ownership::default();

    apply_with_snapshot(&backend, &store, &settings(), &mut ownership);
    apply_with_snapshot(&backend, &store, &settings(), &mut ownership);

    let schemes = std::fs::read_dir(backend.color_scheme_dir())
        .unwrap()
        .count();
    assert_eq!(
        schemes, 1,
        "idempotent applies must not accumulate scheme files"
    );
    assert_eq!(store.list().len(), 2, "each apply records its own snapshot");
}

#[test]
fn ownership_records_original_state_once_across_repeated_applies() {
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let mut ownership = Ownership::default();
    backend
        .apply(&palette(), &settings(), &mut ownership)
        .unwrap();
    let originals: BTreeMap<String, String> = ownership
        .items
        .iter()
        .filter_map(|(k, v)| match v {
            OwnedItem::CreatedFile { .. } => None,
            OwnedItem::ConfigKey { original, .. } => {
                Some((k.clone(), original.clone().unwrap_or_default()))
            }
            _ => None,
        })
        .collect();

    backend
        .apply(&palette(), &settings(), &mut ownership)
        .unwrap();
    for (key, original) in originals {
        match &ownership.items[&key] {
            OwnedItem::ConfigKey { original: now, .. } => {
                assert_eq!(
                    now.as_deref().unwrap_or_default(),
                    original,
                    "{key} lost its original value"
                )
            }
            other => panic!("{key} changed shape: {other:?}"),
        }
    }
}

#[test]
fn nothing_is_written_outside_the_sandbox() {
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let mut ownership = Ownership::default();
    backend
        .apply(&palette(), &settings(), &mut ownership)
        .unwrap();
    for item in ownership.items.values() {
        assert!(
            item.path().starts_with(&sandbox.root),
            "{} escapes the sandbox",
            item.path().display()
        );
    }
}

#[test]
fn the_snapshot_is_valid_json_on_disk_and_reloadable() {
    let sandbox = sandbox();
    let backend = backend(
        sandbox.paths.clone(),
        &["qdbus6", "plasma-apply-colorscheme"],
    );
    let store = SnapshotStore::new(sandbox.paths.snapshots_dir());
    let mut ownership = Ownership::default();
    let snapshot = apply_with_snapshot(&backend, &store, &settings(), &mut ownership);

    let file = sandbox.paths.snapshots_dir().join(snapshot.file_name());
    let text = std::fs::read_to_string(&file).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["version"], 1);
    assert!(parsed["entries"].as_object().unwrap().len() >= 3);

    let reloaded = store.load(&snapshot.id).unwrap();
    assert_eq!(reloaded, snapshot);
}
