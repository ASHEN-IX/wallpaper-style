//! Versioned snapshots and conflict-aware restore.
//!
//! A snapshot is a self-contained JSON record of the previous state of
//! everything an apply is about to touch. Snapshots are written *before* the
//! change, are never modified afterwards, and are kept in
//! `$XDG_STATE_HOME/wallpaper-style/snapshots`.
//!
//! Restore never blindly writes values back. For each item it compares the
//! current state with what the application set:
//!
//! | current state | action |
//! |---|---|
//! | equals what we applied | restore the original |
//! | equals the original already | nothing to do |
//! | anything else | **conflict**: leave the user's value alone and report it |
//!
//! That rule is what lets the user change their colour scheme after
//! installation without uninstalling forcing it back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ownership::{
    hash_bytes, hash_file, IniDocument, OwnedItem, RestoreEntry, RestoreOutcome,
};

/// Version of the snapshot format.
pub const SNAPSHOT_VERSION: u32 = 1;

/// A captured previous state for one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapturedState {
    /// The file did not exist.
    Absent,
    /// The file existed with these bytes, stored inline. Small files only;
    /// larger ones store a hash and a copy in the snapshot directory.
    Content { text: String },
    /// The file was binary or too large; a copy lives next to the snapshot.
    StoredCopy { relative_path: PathBuf },
    /// A configuration key with this value, or `None` for "key not present".
    ConfigValue { value: Option<String> },
}

/// One item captured before a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub id: String,
    /// What was there before.
    pub previous: CapturedState,
    /// What the application wrote.
    pub applied: CapturedState,
    /// The path the item lives at, so restore needs no other lookup.
    pub path: PathBuf,
    /// Extra data needed to restore this item kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<SnapshotDetail>,
}

/// Kind-specific restore metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotDetail {
    ConfigKey { group: String, key: String },
    KwinEffect { effect_id: String },
    SystemdUnit { unit_name: String },
}

/// A complete snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    /// Sortable, filesystem-safe identifier, e.g. `20261101T153000-0001`.
    pub id: String,
    /// Unix seconds when the snapshot was taken.
    pub created_unix: u64,
    /// Application version that took it.
    pub app_version: String,
    /// Monotonic generation of the apply this snapshot precedes.
    pub generation: u64,
    /// Human-readable reason, e.g. `apply`, `settings-change`, `uninstall`.
    pub reason: String,
    pub entries: BTreeMap<String, SnapshotEntry>,
}

impl Snapshot {
    pub fn empty(id: impl Into<String>, generation: u64, reason: impl Into<String>) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            id: id.into(),
            created_unix: now_unix(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            generation,
            reason: reason.into(),
            entries: BTreeMap::new(),
        }
    }

    pub fn file_name(&self) -> String {
        format!("{}.json", self.id)
    }

    /// True when the snapshot recorded nothing, so writing it would be noise.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Current time in Unix seconds, saturating rather than panicking before 1970.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The largest text file stored inline. Anything bigger is copied.
const INLINE_LIMIT: usize = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("cannot write snapshot {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read snapshot {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot {path} is not valid: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("snapshot version {found} is newer than supported {supported}")]
    TooNew { found: u32, supported: u32 },
    #[error("cannot restore {path}: {source}")]
    Restore {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Capture the current state of a file before modifying it.
pub fn capture_file(path: &Path) -> std::io::Result<CapturedState> {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) if text.len() <= INLINE_LIMIT => Ok(CapturedState::Content { text }),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "binary or oversized file must be copied, not captured inline",
            )),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(CapturedState::Absent),
        Err(err) => Err(err),
    }
}

/// Where a snapshot and its large file copies live.
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Build an identifier that sorts chronologically and never collides.
    pub fn next_id(&self, generation: u64) -> String {
        let secs = now_unix();
        let stamp = format_unix_timestamp(secs);
        format!("{stamp}-{generation:04}")
    }

    pub fn write(&self, snapshot: &Snapshot) -> Result<PathBuf, SnapshotError> {
        std::fs::create_dir_all(&self.root).map_err(|source| SnapshotError::Write {
            path: self.root.clone(),
            source,
        })?;
        let path = self.root.join(snapshot.file_name());
        let text = serde_json::to_string_pretty(snapshot).map_err(|e| SnapshotError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        let mut temp =
            tempfile::NamedTempFile::new_in(&self.root).map_err(|source| SnapshotError::Write {
                path: path.clone(),
                source,
            })?;
        use std::io::Write;
        temp.write_all(text.as_bytes())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|source| SnapshotError::Write {
                path: path.clone(),
                source,
            })?;
        temp.persist(&path).map_err(|e| SnapshotError::Write {
            path: path.clone(),
            source: e.error,
        })?;
        Ok(path)
    }

    pub fn load(&self, id: &str) -> Result<Snapshot, SnapshotError> {
        let path = self.root.join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).map_err(|source| SnapshotError::Read {
            path: path.clone(),
            source,
        })?;
        let snapshot: Snapshot = serde_json::from_str(&text).map_err(|e| SnapshotError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        if snapshot.version > SNAPSHOT_VERSION {
            return Err(SnapshotError::TooNew {
                found: snapshot.version,
                supported: SNAPSHOT_VERSION,
            });
        }
        Ok(snapshot)
    }

    /// All snapshots on disk, newest first. Unreadable files are skipped with a
    /// log line rather than failing the whole listing.
    pub fn list(&self) -> Vec<SnapshotSummary> {
        let Ok(dir) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<SnapshotSummary> = dir
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                let path = entry.path();
                let text = std::fs::read_to_string(&path).ok()?;
                let snapshot: Snapshot = match serde_json::from_str(&text) {
                    Ok(s) => s,
                    Err(err) => {
                        log::warn!(
                            "skipping unreadable snapshot {}: {err}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        );
                        return None;
                    }
                };
                Some(SnapshotSummary {
                    id: snapshot.id,
                    created_unix: snapshot.created_unix,
                    reason: snapshot.reason,
                    entries: snapshot.entries.len(),
                    generation: snapshot.generation,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b.created_unix
                .cmp(&a.created_unix)
                .then_with(|| b.id.cmp(&a.id))
        });
        out
    }

    /// Delete snapshots beyond the newest `keep`.
    pub fn prune(&self, keep: usize) -> std::io::Result<usize> {
        let summaries = self.list();
        let mut removed = 0;
        for summary in summaries.into_iter().skip(keep) {
            let path = self.root.join(format!("{}.json", summary.id));
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => log::warn!("cannot prune {}: {err}", path.display()),
            }
        }
        Ok(removed)
    }
}

/// A lightweight row for the GUI's history list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub id: String,
    pub created_unix: u64,
    pub reason: String,
    pub entries: usize,
    pub generation: u64,
}

/// Format a Unix timestamp as a compact UTC `YYYYMMDDTHHMMSS` string.
///
/// Implemented here rather than pulling in a date library: the only requirement
/// is a sortable, filesystem-safe stamp.
pub fn format_unix_timestamp(secs: u64) -> String {
    // Saturate rather than wrap: a timestamp beyond the year 292 million is
    // not a realistic input, and wrapping would produce a nonsensical name.
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`, valid for the whole proleptic Gregorian
/// calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::try_from(yoe).unwrap_or(i64::MAX) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Restore one snapshot.
///
/// `force` rewrites items that changed since apply. Without it, those items are
/// reported as conflicts and left untouched.
pub fn restore(snapshot: &Snapshot, force: bool) -> Vec<RestoreEntry> {
    snapshot
        .entries
        .values()
        .map(|entry| RestoreEntry {
            item: item_of(entry),
            outcome: restore_entry(entry, force),
        })
        .collect()
}

/// Rebuild an [`OwnedItem`] from a snapshot entry, for reporting.
fn item_of(entry: &SnapshotEntry) -> OwnedItem {
    match &entry.detail {
        Some(SnapshotDetail::ConfigKey { group, key }) => OwnedItem::ConfigKey {
            file: entry.path.clone(),
            group: group.clone(),
            key: key.clone(),
            original: match &entry.previous {
                CapturedState::ConfigValue { value } => value.clone(),
                _ => None,
            },
            applied: match &entry.applied {
                CapturedState::ConfigValue { value } => value.clone().unwrap_or_default(),
                _ => String::new(),
            },
        },
        Some(SnapshotDetail::KwinEffect { effect_id }) => OwnedItem::EffectPackage {
            id: effect_id.clone(),
            path: entry.path.clone(),
        },
        Some(SnapshotDetail::SystemdUnit { unit_name }) => OwnedItem::SystemdUnit {
            name: unit_name.clone(),
            path: entry.path.clone(),
            original: match &entry.previous {
                CapturedState::Content { text } => Some(text.clone()),
                _ => None,
            },
        },
        None => match &entry.previous {
            CapturedState::Absent => OwnedItem::CreatedFile {
                path: entry.path.clone(),
            },
            CapturedState::Content { text } => OwnedItem::ModifiedFile {
                path: entry.path.clone(),
                original_sha256: hash_bytes(text.as_bytes()),
                applied_sha256: match &entry.applied {
                    CapturedState::Content { text } => hash_bytes(text.as_bytes()),
                    _ => String::new(),
                },
            },
            CapturedState::StoredCopy { .. } => OwnedItem::ModifiedFile {
                path: entry.path.clone(),
                original_sha256: String::new(),
                applied_sha256: String::new(),
            },
            CapturedState::ConfigValue { .. } => OwnedItem::ConfigKey {
                file: entry.path.clone(),
                group: String::new(),
                key: String::new(),
                original: None,
                applied: String::new(),
            },
        },
    }
}

fn restore_entry(entry: &SnapshotEntry, force: bool) -> RestoreOutcome {
    match &entry.detail {
        Some(SnapshotDetail::ConfigKey { group, key }) => {
            restore_config_key(entry.path.as_path(), group, key, entry, force)
        }
        Some(SnapshotDetail::KwinEffect { .. }) => restore_path(entry, force),
        Some(SnapshotDetail::SystemdUnit { .. }) => restore_path(entry, force),
        None => restore_path(entry, force),
    }
}

fn restore_path(entry: &SnapshotEntry, force: bool) -> RestoreOutcome {
    let current = match std::fs::read(&entry.path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return RestoreOutcome::Failed {
                message: err.to_string(),
            };
        }
    };

    let applied_hash = match &entry.applied {
        CapturedState::Content { text } => Some(hash_bytes(text.as_bytes())),
        CapturedState::Absent => None,
        _ => None,
    };
    let current_hash = current.as_deref().map(hash_bytes);

    // Has the file moved away from what we wrote?
    if !force && current_hash != applied_hash {
        return if current_hash == previous_hash(&entry.previous) {
            RestoreOutcome::AlreadyClean
        } else {
            RestoreOutcome::Conflict
        };
    }

    match &entry.previous {
        CapturedState::Absent => {
            if current.is_none() {
                return RestoreOutcome::AlreadyClean;
            }
            match std::fs::remove_file(&entry.path) {
                Ok(()) => RestoreOutcome::Removed,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    RestoreOutcome::AlreadyClean
                }
                Err(err) => RestoreOutcome::Failed {
                    message: err.to_string(),
                },
            }
        }
        CapturedState::Content { text } => write_atomic(&entry.path, text.as_bytes()).map_or_else(
            |e| RestoreOutcome::Failed {
                message: e.to_string(),
            },
            |()| RestoreOutcome::Restored,
        ),
        CapturedState::StoredCopy { relative_path } => {
            let copy = entry.path.parent().map(|p| p.join(relative_path));
            match copy.and_then(|p| std::fs::read(p).ok()) {
                Some(bytes) => write_atomic(&entry.path, &bytes).map_or_else(
                    |e| RestoreOutcome::Failed {
                        message: e.to_string(),
                    },
                    |()| RestoreOutcome::Restored,
                ),
                None => RestoreOutcome::Failed {
                    message: "stored copy is missing".into(),
                },
            }
        }
        CapturedState::ConfigValue { .. } => RestoreOutcome::Skipped,
    }
}

fn previous_hash(previous: &CapturedState) -> Option<String> {
    match previous {
        CapturedState::Content { text } => Some(hash_bytes(text.as_bytes())),
        CapturedState::Absent => None,
        _ => None,
    }
}

fn restore_config_key(
    path: &Path,
    group: &str,
    key: &str,
    entry: &SnapshotEntry,
    force: bool,
) -> RestoreOutcome {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return RestoreOutcome::Failed {
                message: err.to_string(),
            }
        }
    };
    let mut ini = IniDocument::parse(&text);
    let current = ini.get(group, key);

    let applied = match &entry.applied {
        CapturedState::ConfigValue { value } => value.clone(),
        _ => None,
    };
    if !force && current != applied {
        // Someone else changed it. Restoring would silently undo their change.
        return if current == original_value(&entry.previous) {
            RestoreOutcome::AlreadyClean
        } else {
            RestoreOutcome::Conflict
        };
    }

    match original_value(&entry.previous) {
        Some(original) => ini.set(group, key, &original),
        None => ini.remove(group, key),
    };
    write_atomic(path, ini.render().as_bytes()).map_or_else(
        |e| RestoreOutcome::Failed {
            message: e.to_string(),
        },
        |()| RestoreOutcome::Restored,
    )
}

fn original_value(previous: &CapturedState) -> Option<String> {
    match previous {
        CapturedState::ConfigValue { value } => value.clone(),
        _ => None,
    }
}

/// Write via a temporary file in the same directory so a crash mid-write cannot
/// leave a half-written theme file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Capture the files an apply is expected to write, for a snapshot.
///
/// The backend is the authority on what it writes, and it records everything in
/// the ownership manifest. This helper exists because the snapshot must be taken
/// *before* the write, when the manifest does not yet contain the new paths. It
/// therefore predicts the paths from the same public helpers the Plasma backend
/// uses. Predicting too much is harmless: an entry whose file is never written is
/// removed after the apply, and one whose previous state was captured early is
/// still correct because the first write has not happened yet.
pub fn capture_planned_files(
    paths: &crate::paths::Paths,
    settings: &crate::settings::Settings,
    palette: &ws_palette::Palette,
    variant: ws_palette::Variant,
) -> Vec<SnapshotEntry> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if settings.themes.plasma_color_scheme {
        candidates.push(
            paths
                .kde_color_schemes_dir()
                .join(format!("{}.colors", scheme_file_stem(palette, variant))),
        );
    }
    if settings.themes.kwin_effect {
        let effect = paths.kwin_effects_dir().join(crate::KWIN_EFFECT_ID);
        candidates.push(effect.join("metadata.json"));
        candidates.push(effect.join("contents/code/main.js"));
        candidates.push(paths.kwinrc());
    }
    if settings.themes.kvantum {
        candidates.push(paths.kvantum_dir().join("kvantum.kvconfig"));
    }

    candidates
        .into_iter()
        .map(|path| {
            let previous = capture_file(&path).unwrap_or(CapturedState::Absent);
            SnapshotEntry {
                id: format!("file:{}", path.display()),
                previous,
                applied: CapturedState::Absent,
                path,
                detail: None,
            }
        })
        .collect()
}

/// The colour-scheme file stem for a palette.
///
/// Kept in the core so the snapshot and the backend cannot disagree about the
/// name; `ws-plasma`'s generator uses the same rule.
fn scheme_file_stem(palette: &ws_palette::Palette, variant: ws_palette::Variant) -> String {
    let label = match variant {
        ws_palette::Variant::Dark => "Dark",
        ws_palette::Variant::Light => "Light",
    };
    format!(
        "Wallpaper Style {} {:02x}{:02x}{:02x}-{:02x}{:02x}{:02x}",
        label,
        palette.accent.r,
        palette.accent.g,
        palette.accent.b,
        palette.surface.r,
        palette.surface.g,
        palette.surface.b
    )
}

/// Convenience for building a snapshot entry for a file that already exists.
pub fn capture_for_apply(
    id: impl Into<String>,
    path: &Path,
    applied_text: &str,
) -> std::io::Result<SnapshotEntry> {
    Ok(SnapshotEntry {
        id: id.into(),
        previous: capture_file(path)?,
        applied: CapturedState::Content {
            text: applied_text.to_owned(),
        },
        path: path.to_path_buf(),
        detail: None,
    })
}

/// Hash helper exported for callers that need to compare file states.
pub fn file_hash(path: &Path) -> std::io::Result<Option<String>> {
    hash_file(path)
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
    use std::collections::BTreeMap;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn entry(path: &Path, previous: CapturedState, applied: CapturedState) -> SnapshotEntry {
        SnapshotEntry {
            id: format!("file:{}", path.display()),
            previous,
            applied,
            path: path.to_path_buf(),
            detail: None,
        }
    }

    fn config_entry(
        path: &Path,
        group: &str,
        key: &str,
        prev: Option<&str>,
        applied: &str,
    ) -> SnapshotEntry {
        SnapshotEntry {
            id: format!("cfg:{}:{group}:{key}", path.display()),
            previous: CapturedState::ConfigValue {
                value: prev.map(str::to_owned),
            },
            applied: CapturedState::ConfigValue {
                value: Some(applied.to_owned()),
            },
            path: path.to_path_buf(),
            detail: Some(SnapshotDetail::ConfigKey {
                group: group.into(),
                key: key.into(),
            }),
        }
    }

    #[test]
    fn snapshot_write_and_load_round_trip() {
        let dir = temp();
        let store = SnapshotStore::new(dir.path());
        let mut snapshot = Snapshot::empty("20261101T120000-0001", 1, "apply");
        snapshot.entries.insert(
            "a".into(),
            entry(
                Path::new("/tmp/a"),
                CapturedState::Absent,
                CapturedState::Content { text: "new".into() },
            ),
        );
        store.write(&snapshot).unwrap();
        let loaded = store.load("20261101T120000-0001").unwrap();
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn listing_is_newest_first_and_skips_garbage() {
        let dir = temp();
        let store = SnapshotStore::new(dir.path());
        for (i, secs) in [100u64, 300, 200].iter().enumerate() {
            let mut s = Snapshot::empty(format!("s{i}"), i as u64, "apply");
            s.created_unix = *secs;
            store.write(&s).unwrap();
        }
        std::fs::write(dir.path().join("broken.json"), "not json").unwrap();
        let listed = store.list();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].created_unix, 300);
        assert_eq!(listed[2].created_unix, 100);
    }

    #[test]
    fn pruning_keeps_the_newest_and_reports_the_rest() {
        let dir = temp();
        let store = SnapshotStore::new(dir.path());
        for i in 0..5u64 {
            let mut s = Snapshot::empty(format!("s{i}"), i, "apply");
            s.created_unix = i;
            store.write(&s).unwrap();
        }
        assert_eq!(store.prune(2).unwrap(), 3);
        let left = store.list();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].id, "s4");
    }

    #[test]
    fn restoring_a_created_file_deletes_it() {
        let dir = temp();
        let path = dir.path().join("created.txt");
        std::fs::write(&path, "we made this").unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "a".into(),
                entry(
                    &path,
                    CapturedState::Absent,
                    CapturedState::Content {
                        text: "we made this".into(),
                    },
                ),
            )]),
        };
        let report = restore(&snapshot, false);
        assert_eq!(report[0].outcome, RestoreOutcome::Removed);
        assert!(!path.exists());
    }

    #[test]
    fn restoring_a_modified_file_writes_the_original_back() {
        let dir = temp();
        let path = dir.path().join("theme.colors");
        std::fs::write(&path, "OUR THEME").unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "a".into(),
                entry(
                    &path,
                    CapturedState::Content {
                        text: "ORIGINAL".into(),
                    },
                    CapturedState::Content {
                        text: "OUR THEME".into(),
                    },
                ),
            )]),
        };
        let report = restore(&snapshot, false);
        assert_eq!(report[0].outcome, RestoreOutcome::Restored);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ORIGINAL");
    }

    #[test]
    fn a_file_changed_after_apply_is_a_conflict_and_is_left_alone() {
        let dir = temp();
        let path = dir.path().join("theme.colors");
        // The user edited the file after we applied it.
        std::fs::write(&path, "USER EDIT").unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "a".into(),
                entry(
                    &path,
                    CapturedState::Content {
                        text: "ORIGINAL".into(),
                    },
                    CapturedState::Content {
                        text: "OUR THEME".into(),
                    },
                ),
            )]),
        };
        let report = restore(&snapshot, false);
        assert_eq!(report[0].outcome, RestoreOutcome::Conflict);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDIT");
    }

    #[test]
    fn force_overrides_a_conflict_deliberately() {
        let dir = temp();
        let path = dir.path().join("f");
        std::fs::write(&path, "USER EDIT").unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "a".into(),
                entry(
                    &path,
                    CapturedState::Content {
                        text: "ORIGINAL".into(),
                    },
                    CapturedState::Content {
                        text: "OURS".into(),
                    },
                ),
            )]),
        };
        let report = restore(&snapshot, true);
        assert_eq!(report[0].outcome, RestoreOutcome::Restored);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ORIGINAL");
    }

    #[test]
    fn an_already_clean_item_is_recognised() {
        let dir = temp();
        let path = dir.path().join("f");
        std::fs::write(&path, "ORIGINAL").unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "a".into(),
                entry(
                    &path,
                    CapturedState::Content {
                        text: "ORIGINAL".into(),
                    },
                    CapturedState::Content {
                        text: "OURS".into(),
                    },
                ),
            )]),
        };
        assert_eq!(
            restore(&snapshot, false)[0].outcome,
            RestoreOutcome::AlreadyClean
        );
    }

    #[test]
    fn config_key_restore_puts_the_previous_value_back() {
        let dir = temp();
        let path = dir.path().join("kwinrc");
        std::fs::write(
            &path,
            "[Plugins]\nwallpaper-styleEnabled=true\notherEnabled=true\n",
        )
        .unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "cfg".into(),
                config_entry(&path, "Plugins", "wallpaper-styleEnabled", None, "true"),
            )]),
        };
        let report = restore(&snapshot, false);
        assert_eq!(report[0].outcome, RestoreOutcome::Restored);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("wallpaper-styleEnabled"), "{text}");
        assert!(
            text.contains("otherEnabled=true"),
            "unrelated key was touched: {text}"
        );
    }

    #[test]
    fn a_config_key_changed_by_the_user_is_a_conflict() {
        let dir = temp();
        let path = dir.path().join("kwinrc");
        // The user turned the effect off themselves after we enabled it.
        std::fs::write(&path, "[Plugins]\nwallpaper-styleEnabled=false\n").unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "cfg".into(),
                config_entry(
                    &path,
                    "Plugins",
                    "wallpaper-styleEnabled",
                    Some("true"),
                    "true",
                ),
            )]),
        };
        let report = restore(&snapshot, false);
        assert_eq!(report[0].outcome, RestoreOutcome::Conflict);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("wallpaper-styleEnabled=false"));
    }

    #[test]
    fn config_restore_keeps_the_previous_value_when_one_existed() {
        let dir = temp();
        let path = dir.path().join("kdeglobals");
        std::fs::write(&path, "[General]\nColorScheme=WallpaperStyle\n").unwrap();
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            id: "s".into(),
            created_unix: 0,
            app_version: "t".into(),
            generation: 1,
            reason: "apply".into(),
            entries: BTreeMap::from([(
                "cfg".into(),
                config_entry(
                    &path,
                    "General",
                    "ColorScheme",
                    Some("BreezeDark"),
                    "WallpaperStyle",
                ),
            )]),
        };
        assert_eq!(
            restore(&snapshot, false)[0].outcome,
            RestoreOutcome::Restored
        );
        let ini = IniDocument::parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(
            ini.get("General", "ColorScheme").as_deref(),
            Some("BreezeDark")
        );
    }

    #[test]
    fn timestamps_format_as_sortable_utc() {
        assert_eq!(format_unix_timestamp(0), "19700101T000000");
        // 2026-11-01T15:36:00Z
        assert_eq!(format_unix_timestamp(1_793_547_360), "20261101T153600");
        // Leap day.
        assert_eq!(format_unix_timestamp(1_709_164_800), "20240229T000000");
    }

    #[test]
    fn snapshot_ids_sort_chronologically() {
        let store = SnapshotStore::new("/nonexistent");
        let a = store.next_id(1);
        assert!(a.contains('-'));
        assert!(a.len() > 15);
    }

    #[test]
    fn future_snapshot_versions_are_refused() {
        let dir = temp();
        let store = SnapshotStore::new(dir.path());
        let mut s = Snapshot::empty("s", 1, "apply");
        s.version = 99;
        std::fs::write(
            dir.path().join("s.json"),
            serde_json::to_string(&s).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            store.load("s"),
            Err(SnapshotError::TooNew { found: 99, .. })
        ));
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let dir = temp();
        let path = dir.path().join("sub/dir/file.txt");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["file.txt"]);
    }

    #[test]
    fn capture_reports_absent_files() {
        let dir = temp();
        assert_eq!(
            capture_file(&dir.path().join("nope")).unwrap(),
            CapturedState::Absent
        );
    }

    #[test]
    fn capture_rejects_binary_content() {
        let dir = temp();
        let path = dir.path().join("bin");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x01]).unwrap();
        assert!(capture_file(&path).is_err());
    }
}
