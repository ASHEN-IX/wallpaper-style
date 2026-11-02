//! Ownership tracking and recovery.
//!
//! Two problems have to be solved together:
//!
//! 1. **Restore must put things back.** Before the application changes anything
//!    it records the previous value, so uninstalling can undo exactly what it
//!    did.
//! 2. **Restore must not clobber unrelated changes.** If the user picked a
//!    different colour scheme after installing, uninstalling must not force the
//!    old one back.
//!
//! Both are handled by recording, for every thing touched, three pieces of
//! information: what it was *before* we first touched it, what we set it *to*,
//! and what it is *now*. Restore compares the last two:
//!
//! * still what we set → write the original back;
//! * changed since → leave it alone and report a conflict, unless the caller
//!   explicitly asks to force.
//!
//! The manifest is versioned like the settings file so an older manifest can
//! still be read and restored after an upgrade.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Version of the ownership manifest format.
pub const OWNERSHIP_VERSION: u32 = 1;

/// One thing the application changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OwnedItem {
    /// A file we created. Removing is safe once the content still matches.
    CreatedFile { path: PathBuf },
    /// A file that existed and whose content we replaced.
    ModifiedFile {
        path: PathBuf,
        original_sha256: String,
        applied_sha256: String,
    },
    /// A key inside a KDE configuration file.
    ConfigKey {
        file: PathBuf,
        group: String,
        key: String,
        original: Option<String>,
        applied: String,
    },
    /// A KWin effect package directory we installed.
    EffectPackage { id: String, path: PathBuf },
    /// A systemd user unit we installed.
    SystemdUnit {
        name: String,
        path: PathBuf,
        original: Option<String>,
    },
}

impl OwnedItem {
    /// The path this item lives at.
    pub fn path(&self) -> &Path {
        match self {
            OwnedItem::CreatedFile { path }
            | OwnedItem::ModifiedFile { path, .. }
            | OwnedItem::ConfigKey { file: path, .. }
            | OwnedItem::EffectPackage { path, .. }
            | OwnedItem::SystemdUnit { path, .. } => path,
        }
    }

    /// A short description for the UI and for `status` output.
    pub fn describe(&self) -> String {
        match self {
            OwnedItem::CreatedFile { path } => format!("created {}", path.display()),
            OwnedItem::ModifiedFile { path, .. } => format!("modified {}", path.display()),
            OwnedItem::ConfigKey {
                file, group, key, ..
            } => {
                format!("{}/{}/{}", file.display(), group, key)
            }
            OwnedItem::EffectPackage { id, path } => {
                format!("KWin effect {id} at {}", path.display())
            }
            OwnedItem::SystemdUnit { name, .. } => format!("systemd unit {name}"),
        }
    }
}

/// The full record of what the application owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ownership {
    pub version: u32,
    /// Monotonic counter, incremented on every successful apply. Used to pair
    /// snapshots with the state they describe.
    pub applied_generation: u64,
    /// Items we changed, keyed by a stable identifier so repeated applies update
    /// rather than duplicate entries.
    pub items: BTreeMap<String, OwnedItem>,
    /// Snapshot identifiers, newest last.
    pub snapshots: Vec<String>,
}

impl Default for Ownership {
    fn default() -> Self {
        Self {
            version: OWNERSHIP_VERSION,
            applied_generation: 0,
            items: BTreeMap::new(),
            snapshots: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OwnershipError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a valid ownership record: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("ownership record version {found} is newer than supported {supported}")]
    TooNew { found: u32, supported: u32 },
}

impl Ownership {
    pub fn load(path: &Path) -> Result<Self, OwnershipError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(OwnershipError::Read {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        let mut record: Self = serde_json::from_str(&text).map_err(|e| OwnershipError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        if record.version > OWNERSHIP_VERSION {
            return Err(OwnershipError::TooNew {
                found: record.version,
                supported: OWNERSHIP_VERSION,
            });
        }
        if record.version == 0 {
            record.version = 1;
        }
        Ok(record)
    }

    pub fn save(&self, path: &Path) -> Result<(), OwnershipError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| OwnershipError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| OwnershipError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut temp =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| OwnershipError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        use std::io::Write;
        temp.write_all(text.as_bytes())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|source| OwnershipError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        temp.persist(path).map_err(|e| OwnershipError::Write {
            path: path.to_path_buf(),
            source: e.error,
        })?;
        Ok(())
    }

    /// Record an item, keeping the *original* state if it was already recorded.
    ///
    /// This is what makes restore correct across repeated applies: the second
    /// apply of the same file must not overwrite "what it looked like before we
    /// ever touched it" with "what we set it to last time".
    pub fn record(&mut self, id: impl Into<String>, item: OwnedItem) {
        let id = id.into();
        match (self.items.get(&id), &item) {
            // Already tracked as modified: keep the original hash, update applied.
            (
                Some(OwnedItem::ModifiedFile {
                    original_sha256, ..
                }),
                OwnedItem::ModifiedFile { applied_sha256, .. },
            ) => {
                self.items.insert(
                    id,
                    OwnedItem::ModifiedFile {
                        path: item.path().to_path_buf(),
                        original_sha256: original_sha256.clone(),
                        applied_sha256: applied_sha256.clone(),
                    },
                );
            }
            // A file we created is still one we created.
            (Some(OwnedItem::CreatedFile { .. }), OwnedItem::ModifiedFile { .. }) => {
                // Leave the CreatedFile entry alone: it is ours end to end.
            }
            // Already-modified config key: keep the original value.
            (
                Some(OwnedItem::ConfigKey { original, .. }),
                OwnedItem::ConfigKey {
                    file,
                    group,
                    key,
                    applied,
                    ..
                },
            ) => {
                self.items.insert(
                    id,
                    OwnedItem::ConfigKey {
                        file: file.clone(),
                        group: group.clone(),
                        key: key.clone(),
                        original: original.clone(),
                        applied: applied.clone(),
                    },
                );
            }
            (
                Some(OwnedItem::SystemdUnit { original, .. }),
                OwnedItem::SystemdUnit { name, path, .. },
            ) => {
                self.items.insert(
                    id,
                    OwnedItem::SystemdUnit {
                        name: name.clone(),
                        path: path.clone(),
                        original: original.clone(),
                    },
                );
            }
            _ => {
                self.items.insert(id, item);
            }
        }
    }

    /// True when the application has recorded changes for the given path.
    pub fn owns_path(&self, path: &Path) -> bool {
        self.items.values().any(|item| item.path() == path)
    }

    /// Items grouped by the file they affect, for a readable status report.
    pub fn by_path(&self) -> BTreeMap<&Path, Vec<&OwnedItem>> {
        let mut map: BTreeMap<&Path, Vec<&OwnedItem>> = BTreeMap::new();
        for item in self.items.values() {
            map.entry(item.path()).or_default().push(item);
        }
        map
    }
}

/// What restore decided for one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RestoreOutcome {
    /// The original value was written back.
    Restored,
    /// The file was deleted.
    Removed,
    /// Something changed it since we applied; left alone.
    Conflict,
    /// It was already in the original state.
    AlreadyClean,
    /// Restoring failed; the desktop was left untouched.
    Failed { message: String },
    /// Skipped by request.
    Skipped,
}

/// A per-item restore report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreEntry {
    pub item: OwnedItem,
    pub outcome: RestoreOutcome,
}

/// Compute the content hash used to detect drift.
pub fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Hash a file, or `None` when it does not exist.
pub fn hash_file(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(hash_bytes(&bytes))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Read and edit a KDE INI file without destroying comments or ordering.
///
/// Plasma and KWin configuration files are hand-editable and full of comments;
/// a naive parse-and-rewrite loses them. This editor keeps every line it does
/// not need to change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IniDocument {
    lines: Vec<String>,
}

impl IniDocument {
    pub fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
        }
    }

    pub fn render(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    fn group_bounds(&self, group: &str) -> Option<(usize, usize)> {
        let header = format!("[{group}]");
        let start = self.lines.iter().position(|line| line.trim() == header)?;
        let end = self.lines[start + 1..]
            .iter()
            .position(|line| {
                let trimmed = line.trim();
                trimmed.starts_with('[') && trimmed.ends_with(']')
            })
            .map_or(self.lines.len(), |offset| start + 1 + offset);
        Some((start, end))
    }

    pub fn get(&self, group: &str, key: &str) -> Option<String> {
        let (start, end) = self.group_bounds(group)?;
        self.lines[start + 1..end].iter().find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_owned())
        })
    }

    /// Set a key, creating the group if necessary. Returns whether the
    /// document changed.
    pub fn set(&mut self, group: &str, key: &str, value: &str) -> bool {
        let new_line = format!("{key}={value}");
        match self.group_bounds(group) {
            Some((start, end)) => {
                for line in &mut self.lines[start + 1..end] {
                    if let Some((k, _)) = line.split_once('=') {
                        if k.trim() == key {
                            if *line == new_line {
                                return false;
                            }
                            *line = new_line;
                            return true;
                        }
                    }
                }
                self.lines.insert(end, new_line);
                true
            }
            None => {
                if !self.lines.is_empty() && self.lines.last().is_some_and(|l| !l.trim().is_empty())
                {
                    self.lines.push(String::new());
                }
                self.lines.push(format!("[{group}]"));
                self.lines.push(new_line);
                true
            }
        }
    }

    /// Remove a key entirely. Returns whether anything was removed.
    pub fn remove(&mut self, group: &str, key: &str) -> bool {
        let Some((start, end)) = self.group_bounds(group) else {
            return false;
        };
        if let Some(index) = self.lines[start + 1..end]
            .iter()
            .position(|line| line.split_once('=').is_some_and(|(k, _)| k.trim() == key))
        {
            self.lines.remove(start + 1 + index);
            return true;
        }
        false
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

    #[test]
    fn a_fresh_record_is_empty_and_current() {
        let record = Ownership::default();
        assert_eq!(record.version, OWNERSHIP_VERSION);
        assert!(record.items.is_empty());
        assert!(!record.owns_path(Path::new("/x")));
    }

    #[test]
    fn record_save_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ownership.json");
        let mut record = Ownership::default();
        record.record(
            "file:/a",
            OwnedItem::CreatedFile {
                path: PathBuf::from("/a"),
            },
        );
        record.record(
            "cfg:kdeglobals/General/ColorScheme",
            OwnedItem::ConfigKey {
                file: PathBuf::from("/cfg/kdeglobals"),
                group: "General".into(),
                key: "ColorScheme".into(),
                original: Some("BreezeDark".into()),
                applied: "WallpaperStyle".into(),
            },
        );
        record.applied_generation = 3;
        record.snapshots.push("snap-1".into());
        record.save(&path).unwrap();
        let loaded = Ownership::load(&path).unwrap();
        assert_eq!(loaded, record);
    }

    #[test]
    fn missing_file_is_an_empty_record() {
        let dir = tempfile::tempdir().unwrap();
        let record = Ownership::load(&dir.path().join("nope.json")).unwrap();
        assert_eq!(record, Ownership::default());
    }

    #[test]
    fn repeated_record_keeps_the_original_state() {
        let mut record = Ownership::default();
        record.record(
            "file:/a",
            OwnedItem::ModifiedFile {
                path: PathBuf::from("/a"),
                original_sha256: "orig".into(),
                applied_sha256: "first".into(),
            },
        );
        // Second apply replaces the content again.
        record.record(
            "file:/a",
            OwnedItem::ModifiedFile {
                path: PathBuf::from("/a"),
                original_sha256: "first".into(),
                applied_sha256: "second".into(),
            },
        );
        match &record.items["file:/a"] {
            OwnedItem::ModifiedFile {
                original_sha256,
                applied_sha256,
                ..
            } => {
                assert_eq!(original_sha256, "orig", "original must survive re-applies");
                assert_eq!(applied_sha256, "second");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn repeated_config_key_record_keeps_the_original_value() {
        let mut record = Ownership::default();
        let item = |original: Option<&str>, applied: &str| OwnedItem::ConfigKey {
            file: PathBuf::from("/k"),
            group: "Plugins".into(),
            key: "xEnabled".into(),
            original: original.map(str::to_owned),
            applied: applied.into(),
        };
        record.record("cfg", item(Some("false"), "true"));
        record.record("cfg", item(Some("true"), "false"));
        match &record.items["cfg"] {
            OwnedItem::ConfigKey {
                original, applied, ..
            } => {
                assert_eq!(original.as_deref(), Some("false"));
                assert_eq!(applied, "false");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_created_file_is_not_downgraded_to_modified() {
        let mut record = Ownership::default();
        record.record(
            "file:/a",
            OwnedItem::CreatedFile {
                path: PathBuf::from("/a"),
            },
        );
        record.record(
            "file:/a",
            OwnedItem::ModifiedFile {
                path: PathBuf::from("/a"),
                original_sha256: "x".into(),
                applied_sha256: "y".into(),
            },
        );
        assert!(matches!(
            record.items["file:/a"],
            OwnedItem::CreatedFile { .. }
        ));
    }

    #[test]
    fn a_future_manifest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ownership.json");
        std::fs::write(
            &path,
            r#"{"version":99,"applied_generation":0,"items":{},"snapshots":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            Ownership::load(&path),
            Err(OwnershipError::TooNew { found: 99, .. })
        ));
    }

    #[test]
    fn corrupt_manifest_is_reported_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ownership.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(
            Ownership::load(&path),
            Err(OwnershipError::Parse { .. })
        ));
    }

    #[test]
    fn hashing_detects_content_drift() {
        assert_eq!(hash_bytes(b"a"), hash_bytes(b"a"));
        assert_ne!(hash_bytes(b"a"), hash_bytes(b"b"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        assert_eq!(hash_file(&path).unwrap(), None);
        std::fs::write(&path, b"contents").unwrap();
        assert_eq!(hash_file(&path).unwrap(), Some(hash_bytes(b"contents")));
    }

    #[test]
    fn ini_get_and_set_preserve_unrelated_content() {
        let text = "# comment\n[General]\nColorScheme=Breeze\n\n[Plugins]\nxEnabled=false\n";
        let mut ini = IniDocument::parse(text);
        assert_eq!(ini.get("General", "ColorScheme").as_deref(), Some("Breeze"));
        assert!(ini.set("Plugins", "xEnabled", "true"));
        assert_eq!(ini.get("Plugins", "xEnabled").as_deref(), Some("true"));
        let rendered = ini.render();
        assert!(rendered.contains("# comment"), "{rendered}");
        assert!(rendered.contains("ColorScheme=Breeze"), "{rendered}");
    }

    #[test]
    fn ini_creates_missing_groups_and_keys() {
        let mut ini = IniDocument::parse("[A]\nx=1\n");
        assert!(ini.set("B", "y", "2"));
        assert_eq!(ini.get("B", "y").as_deref(), Some("2"));
        assert!(ini.set("A", "z", "3"));
        assert_eq!(ini.get("A", "z").as_deref(), Some("3"));
        assert_eq!(ini.get("A", "x").as_deref(), Some("1"));
    }

    #[test]
    fn ini_set_reports_no_change_when_value_is_identical() {
        let mut ini = IniDocument::parse("[A]\nx=1\n");
        assert!(!ini.set("A", "x", "1"));
    }

    #[test]
    fn ini_remove_reports_whether_anything_happened() {
        let mut ini = IniDocument::parse("[A]\nx=1\ny=2\n");
        assert!(ini.remove("A", "x"));
        assert!(!ini.remove("A", "x"));
        assert_eq!(ini.get("A", "y").as_deref(), Some("2"));
    }

    #[test]
    fn ini_does_not_leak_across_sections() {
        let ini = IniDocument::parse("[A]\nx=1\n[B]\nx=2\n");
        assert_eq!(ini.get("A", "x").as_deref(), Some("1"));
        assert_eq!(ini.get("B", "x").as_deref(), Some("2"));
        assert_eq!(ini.get("C", "x"), None);
    }

    #[test]
    fn by_path_groups_items_usefully() {
        let mut record = Ownership::default();
        record.record(
            "a",
            OwnedItem::ConfigKey {
                file: PathBuf::from("/k"),
                group: "Plugins".into(),
                key: "a".into(),
                original: None,
                applied: "1".into(),
            },
        );
        record.record(
            "b",
            OwnedItem::ConfigKey {
                file: PathBuf::from("/k"),
                group: "Plugins".into(),
                key: "b".into(),
                original: None,
                applied: "1".into(),
            },
        );
        let grouped = record.by_path();
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[Path::new("/k")].len(), 2);
    }
}
