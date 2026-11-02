//! Wallpaper selection policy, independent of how the wallpaper was read.
//!
//! The backend crate reports what each monitor currently shows; this module
//! decides which one becomes *the* wallpaper for the palette, and explains the
//! choice. Keeping the policy here means the rules are testable without Plasma
//! and identical across backends.
//!
//! ## Policy
//!
//! * Monitors are considered in **screen order** (Plasma's `lastScreen`), so the
//!   primary/first screen wins a tie. The prototype always did this and it is
//!   the behaviour users expect: the wallpaper they think of as "the desktop".
//! * A slideshow is supported only when the plugin publishes the *currently
//!   displayed* image. When it does not, the wallpaper is reported as
//!   unsupported with an explanation rather than silently frozen on the first
//!   frame.
//! * A monitor whose image is missing, unreadable, or a non-local URL is
//!   skipped in favour of the next monitor that works, and the reason is
//!   recorded.
//! * When nothing usable is found, the reason chain names every monitor so the
//!   user can see *why* detection failed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What Plasma reports for one monitor's wallpaper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorWallpaper {
    /// Plasma's `lastScreen` index.
    pub screen: u32,
    /// The wallpaper plugin id, e.g. `org.kde.image` or `a2n.blur`.
    pub plugin: String,
    /// The raw `Image` configuration value, which is usually a `file://` URL.
    pub image: Option<String>,
    /// Whether the plugin is one the product knows how to read.
    pub plugin_kind: PluginKind,
}

/// How well a wallpaper plugin is understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// `org.kde.image`: a single still image.
    Image,
    /// A slideshow/rotating plugin that publishes the current frame.
    Slideshow,
    /// Third-party plugin, assumed to publish a usable `Image` value.
    ThirdParty,
    /// A plugin known not to expose an image (video, dynamic colour, ...).
    Unsupported,
}

/// Why a particular monitor was not used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Rejection {
    NoImageConfigured,
    UnsupportedPlugin { plugin: String },
    NotALocalFile { value: String },
    MissingFile { path: PathBuf },
    UnreadableFile { path: PathBuf, message: String },
}

impl Rejection {
    pub fn describe(&self) -> String {
        match self {
            Rejection::NoImageConfigured => "no image is configured".to_owned(),
            Rejection::UnsupportedPlugin { plugin } => {
                format!("the wallpaper plugin {plugin} does not publish its current image")
            }
            Rejection::NotALocalFile { value } => {
                format!("{value} is not a local file wallpaper")
            }
            Rejection::MissingFile { path } => format!("{} no longer exists", path.display()),
            Rejection::UnreadableFile { path, message } => {
                format!("{} cannot be read: {message}", path.display())
            }
        }
    }
}

/// The outcome of choosing a wallpaper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Selection {
    /// A usable local image.
    Found {
        path: PathBuf,
        screen: u32,
        plugin: String,
        plugin_kind: PluginKind,
        /// Monitors that were considered and rejected before this one.
        rejected: Vec<(u32, Rejection)>,
    },
    /// Nothing usable.
    None {
        rejected: Vec<(u32, Rejection)>,
        /// True when every monitor was rejected for a *structural* reason
        /// (unsupported plugin, no image) rather than a missing file. The UI
        /// uses this to suggest "pick an image manually".
        suggest_manual: bool,
    },
}

impl Selection {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Selection::Found { path, .. } => Some(path),
            Selection::None { .. } => None,
        }
    }

    pub fn rejected(&self) -> &[(u32, Rejection)] {
        match self {
            Selection::Found { rejected, .. } | Selection::None { rejected, .. } => rejected,
        }
    }

    /// A single sentence explaining a failure, for the CLI and the GUI banner.
    pub fn explain(&self) -> Option<String> {
        match self {
            Selection::Found { .. } => None,
            Selection::None {
                rejected,
                suggest_manual,
            } => {
                if rejected.is_empty() {
                    return Some(
                        "No desktop wallpaper was reported by Plasma. Choose an image manually."
                            .to_owned(),
                    );
                }
                let mut parts: Vec<String> = rejected
                    .iter()
                    .map(|(screen, reason)| format!("screen {screen}: {}", reason.describe()))
                    .collect();
                parts.sort();
                let suffix = if *suggest_manual {
                    " Choose an image manually from the Wallpaper page."
                } else {
                    ""
                };
                Some(format!(
                    "No usable wallpaper found ({}).{suffix}",
                    parts.join("; ")
                ))
            }
        }
    }
}

/// Convert a Plasma `file://` URL to a local path.
///
/// Plasma stores `file:///home/x/a.jpg`, sometimes percent-encoded, and on rare
/// setups `file://localhost/home/x/a.jpg`. Anything else (an HTTP URL, a
/// `desktop:/` path inside a folder view) is not something we can read.
pub fn local_path_from_url(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // Plain absolute paths appear in hand-edited configs.
    if value.starts_with('/') {
        return Some(PathBuf::from(percent_decode(value)));
    }
    let rest = value.strip_prefix("file://")?;
    // Drop an authority component if present.
    let path = if let Some(after) = rest.strip_prefix("localhost/") {
        format!("/{after}")
    } else if rest.starts_with('/') {
        rest.to_owned()
    } else {
        return None;
    };
    if path.is_empty() || path == "/" {
        return None;
    }
    Some(PathBuf::from(percent_decode(&path)))
}

/// Decode `%XX` escapes without a URL crate.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Classify a wallpaper plugin id.
pub fn plugin_kind(plugin: &str) -> PluginKind {
    match plugin {
        "org.kde.image" => PluginKind::Image,
        // These publish a current frame under the same `Image` key.
        "org.kde.slideshow" | "org.kde.knowgo" => PluginKind::Slideshow,
        // Known not to expose a readable still image.
        "org.kde.color" | "org.kde.video" | "com.github.zren.animatedwallpapers" => {
            PluginKind::Unsupported
        }
        _ => PluginKind::ThirdParty,
    }
}

/// Decide which monitor's wallpaper to use.
///
/// `exists` is injected so tests do not need a filesystem; the real caller
/// passes a closure backed by `Path::is_file`.
pub fn select(monitors: &[MonitorWallpaper], exists: impl Fn(&Path) -> bool) -> Selection {
    let mut ordered = monitors.to_vec();
    ordered.sort_by_key(|m| m.screen);

    let mut rejected: Vec<(u32, Rejection)> = Vec::new();
    let mut structural_only = true;

    for monitor in &ordered {
        if monitor.plugin_kind == PluginKind::Unsupported {
            rejected.push((
                monitor.screen,
                Rejection::UnsupportedPlugin {
                    plugin: monitor.plugin.clone(),
                },
            ));
            continue;
        }
        let Some(raw) = monitor.image.as_deref().filter(|v| !v.trim().is_empty()) else {
            rejected.push((monitor.screen, Rejection::NoImageConfigured));
            continue;
        };
        let Some(path) = local_path_from_url(raw) else {
            rejected.push((
                monitor.screen,
                Rejection::NotALocalFile {
                    value: raw.to_owned(),
                },
            ));
            continue;
        };
        // A missing file is a transient problem worth retrying; an unsupported
        // plugin is structural and means the user should pick manually.
        structural_only = false;
        if !exists(&path) {
            rejected.push((monitor.screen, Rejection::MissingFile { path }));
            continue;
        }
        return Selection::Found {
            path,
            screen: monitor.screen,
            plugin: monitor.plugin.clone(),
            plugin_kind: monitor.plugin_kind,
            rejected,
        };
    }

    let suggest_manual = structural_only || ordered.is_empty();
    Selection::None {
        rejected,
        suggest_manual,
    }
}

/// Group monitors by desktop container, which is how Plasma stores them.
/// Provided so the backend can present a stable ordering to [`select`].
pub fn sort_by_screen(monitors: &mut [MonitorWallpaper]) {
    monitors.sort_by_key(|m| m.screen);
}

/// A fingerprint used to detect "same path, different image".
///
/// The prototype used `(path, mtime, size)`. That misses a replaced file whose
/// size and timestamp happen to match, and it also triggers needlessly when a
/// file is merely touched. Hashing a bounded prefix of the file is a better
/// trade: cheap, and sensitive to real content changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperFingerprint {
    pub path: PathBuf,
    pub size: u64,
    /// `None` for files that could not be hashed.
    pub content_prefix_hash: Option<String>,
}

/// How many leading bytes to hash. Enough to distinguish images reliably while
/// keeping a large wallpaper cheap to re-check on every poll.
pub const FINGERPRINT_PREFIX_BYTES: u64 = 64 * 1024;

impl WallpaperFingerprint {
    pub fn of(path: &Path) -> std::io::Result<Self> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        let mut buffer = vec![0u8; FINGERPRINT_PREFIX_BYTES.min(size.max(1)) as usize];
        let read = file.read(&mut buffer)?;
        let mut hasher = Sha256::new();
        hasher.update(&buffer[..read]);
        Ok(Self {
            path: path.to_path_buf(),
            size,
            content_prefix_hash: Some(format!("{:x}", hasher.finalize())),
        })
    }

    /// True when this describes the same image content at the same path.
    pub fn matches(&self, other: &Self) -> bool {
        self.path == other.path
            && self.size == other.size
            && self.content_prefix_hash == other.content_prefix_hash
    }
}

/// Detected problems with a slideshow or multi-monitor arrangement, surfaced in
/// diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiMonitorNote {
    /// Monitors disagree; the palette comes from one of them.
    MixedWallpapers { used_screen: u32, others: Vec<u32> },
    /// A slideshow is configured on the used monitor.
    SlideshowInUse { screen: u32 },
}

/// Notes about a selection, for the diagnostics page.
pub fn notes_for(monitors: &[MonitorWallpaper], selection: &Selection) -> Vec<MultiMonitorNote> {
    let Some(used_screen) = (match selection {
        Selection::Found {
            screen,
            plugin_kind,
            ..
        } => Some((*screen, *plugin_kind)),
        Selection::None { .. } => None,
    }) else {
        return Vec::new();
    };
    let (screen, kind) = used_screen;
    let mut notes = Vec::new();
    let others: Vec<u32> = monitors
        .iter()
        .filter(|m| m.screen != screen)
        .filter(|m| m.image.is_some())
        .map(|m| m.screen)
        .collect();
    if !others.is_empty() {
        notes.push(MultiMonitorNote::MixedWallpapers {
            used_screen: screen,
            others,
        });
    }
    if kind == PluginKind::Slideshow {
        notes.push(MultiMonitorNote::SlideshowInUse { screen });
    }
    notes
}

/// A convenience for the backend: turn raw config sections into monitor records.
///
/// `sections` maps container ids to their raw keys. Plasma stores each desktop
/// container's wallpaper as
/// `[Containments][<id>][Wallpaper][<plugin>][General]`.
#[derive(Debug, Clone, Default)]
pub struct RawWallpaperConfig {
    /// One entry per desktop container, in any order.
    pub containers: Vec<RawContainer>,
}

#[derive(Debug, Clone, Default)]
pub struct RawContainer {
    pub container_id: String,
    pub screen: Option<u32>,
    pub plugin: Option<String>,
    /// Keys from the `[General]` section of the wallpaper group.
    pub general: BTreeMap<String, String>,
}

impl RawWallpaperConfig {
    /// Resolve into [`MonitorWallpaper`] records. Containers without a screen or
    /// plugin are ignored: they are panels, not desktops.
    pub fn into_monitors(self) -> Vec<MonitorWallpaper> {
        self.containers
            .into_iter()
            .filter_map(|container| {
                let plugin = container.plugin?;
                let screen = container.screen?;
                let image = container
                    .general
                    .get("Image")
                    .cloned()
                    .or_else(|| container.general.get("PreviewImage").cloned());
                Some(MonitorWallpaper {
                    screen,
                    plugin_kind: plugin_kind(&plugin),
                    plugin,
                    image,
                })
            })
            .collect()
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

    fn monitor(screen: u32, plugin: &str, image: Option<&str>) -> MonitorWallpaper {
        MonitorWallpaper {
            screen,
            plugin: plugin.to_owned(),
            image: image.map(str::to_owned),
            plugin_kind: plugin_kind(plugin),
        }
    }

    #[test]
    fn file_urls_become_local_paths() {
        assert_eq!(
            local_path_from_url("file:///home/x/Pictures/a.jpg"),
            Some(PathBuf::from("/home/x/Pictures/a.jpg"))
        );
        assert_eq!(
            local_path_from_url("file://localhost/home/x/a.jpg"),
            Some(PathBuf::from("/home/x/a.jpg"))
        );
        assert_eq!(
            local_path_from_url("file:///home/x/My%20Wallpaper.png"),
            Some(PathBuf::from("/home/x/My Wallpaper.png"))
        );
        assert_eq!(
            local_path_from_url("file:///home/x/caf%C3%A9.jpg"),
            Some(PathBuf::from("/home/x/café.jpg"))
        );
        // Hand-edited configs may hold a bare path.
        assert_eq!(
            local_path_from_url("/home/x/a.jpg"),
            Some(PathBuf::from("/home/x/a.jpg"))
        );
    }

    #[test]
    fn non_local_urls_are_rejected() {
        assert_eq!(local_path_from_url("https://example.com/a.jpg"), None);
        assert_eq!(local_path_from_url("http://example.com/a.jpg"), None);
        assert_eq!(local_path_from_url("file://"), None);
        assert_eq!(local_path_from_url("file:///"), None);
        assert_eq!(local_path_from_url(""), None);
        assert_eq!(local_path_from_url("   "), None);
    }

    #[test]
    fn the_lowest_screen_wins() {
        let monitors = vec![
            monitor(1, "org.kde.image", Some("file:///b.jpg")),
            monitor(0, "org.kde.image", Some("file:///a.jpg")),
        ];
        let selection = select(&monitors, |_| true);
        match selection {
            Selection::Found { path, screen, .. } => {
                assert_eq!(screen, 0);
                assert_eq!(path, PathBuf::from("/a.jpg"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_broken_primary_screen_falls_through_to_the_next() {
        let monitors = vec![
            monitor(0, "org.kde.image", Some("file:///missing.jpg")),
            monitor(1, "org.kde.image", Some("file:///ok.jpg")),
        ];
        let selection = select(&monitors, |p| p == Path::new("/ok.jpg"));
        match &selection {
            Selection::Found {
                screen, rejected, ..
            } => {
                assert_eq!(*screen, 1);
                assert_eq!(rejected.len(), 1);
                assert!(matches!(rejected[0].1, Rejection::MissingFile { .. }));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unsupported_plugins_are_reported_with_their_name() {
        let monitors = vec![monitor(0, "org.kde.video", Some("file:///a.mp4"))];
        let selection = select(&monitors, |_| true);
        match selection {
            Selection::None {
                rejected,
                suggest_manual,
            } => {
                assert!(suggest_manual);
                assert!(matches!(
                    &rejected[0].1,
                    Rejection::UnsupportedPlugin { plugin } if plugin == "org.kde.video"
                ));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_third_party_plugin_that_publishes_an_image_works() {
        let monitors = vec![monitor(0, "a2n.blur", Some("file:///a.jpg"))];
        assert!(matches!(
            select(&monitors, |_| true),
            Selection::Found { .. }
        ));
    }

    #[test]
    fn slideshows_are_accepted_and_noted() {
        let monitors = vec![monitor(0, "org.kde.slideshow", Some("file:///frame1.jpg"))];
        let selection = select(&monitors, |_| true);
        assert!(matches!(
            selection,
            Selection::Found {
                plugin_kind: PluginKind::Slideshow,
                ..
            }
        ));
        assert_eq!(
            notes_for(&monitors, &selection),
            vec![MultiMonitorNote::SlideshowInUse { screen: 0 }]
        );
    }

    #[test]
    fn mixed_monitors_are_noted() {
        let monitors = vec![
            monitor(0, "org.kde.image", Some("file:///a.jpg")),
            monitor(1, "org.kde.image", Some("file:///b.jpg")),
        ];
        let selection = select(&monitors, |_| true);
        assert_eq!(
            notes_for(&monitors, &selection),
            vec![MultiMonitorNote::MixedWallpapers {
                used_screen: 0,
                others: vec![1]
            }]
        );
    }

    #[test]
    fn no_monitors_at_all_suggests_manual_selection() {
        let selection = select(&[], |_| true);
        assert!(matches!(
            selection,
            Selection::None {
                suggest_manual: true,
                ..
            }
        ));
        let message = selection.explain().unwrap();
        assert!(message.contains("manually"), "{message}");
    }

    #[test]
    fn a_missing_file_does_not_suggest_manual_selection() {
        let monitors = vec![monitor(0, "org.kde.image", Some("file:///gone.jpg"))];
        let selection = select(&monitors, |_| false);
        match selection {
            Selection::None { suggest_manual, .. } => assert!(
                !suggest_manual,
                "a transiently missing file should be retried, not replaced by a manual choice"
            ),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn the_failure_message_names_every_screen() {
        let monitors = vec![
            monitor(0, "org.kde.color", None),
            monitor(1, "org.kde.image", Some("file:///gone.jpg")),
        ];
        let selection = select(&monitors, |_| false);
        let message = selection.explain().unwrap();
        assert!(message.contains("screen 0"), "{message}");
        assert!(message.contains("screen 1"), "{message}");
    }

    #[test]
    fn fingerprint_detects_a_change_at_the_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wall.jpg");
        std::fs::write(&path, b"first image bytes").unwrap();
        let first = WallpaperFingerprint::of(&path).unwrap();
        assert!(first.matches(&WallpaperFingerprint::of(&path).unwrap()));

        std::fs::write(&path, b"second image bytes!").unwrap();
        let second = WallpaperFingerprint::of(&path).unwrap();
        assert!(
            !first.matches(&second),
            "same path, different content must differ"
        );
    }

    #[test]
    fn a_touch_without_content_change_is_not_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.jpg");
        std::fs::write(&path, b"same bytes").unwrap();
        let first = WallpaperFingerprint::of(&path).unwrap();
        std::fs::write(&path, b"same bytes").unwrap();
        assert!(first.matches(&WallpaperFingerprint::of(&path).unwrap()));
    }

    #[test]
    fn the_same_bytes_at_a_different_path_are_a_different_wallpaper() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(!WallpaperFingerprint::of(&a)
            .unwrap()
            .matches(&WallpaperFingerprint::of(&b).unwrap()));
    }

    #[test]
    fn raw_configs_becomes_monitors_ignoring_panels() {
        let raw = RawWallpaperConfig {
            containers: vec![
                RawContainer {
                    container_id: "73".into(),
                    screen: Some(0),
                    plugin: Some("a2n.blur".into()),
                    general: BTreeMap::from([(
                        "Image".to_owned(),
                        "file:///home/x/a.jpg".to_owned(),
                    )]),
                },
                // A panel: no screen, so it is not a desktop.
                RawContainer {
                    container_id: "74".into(),
                    ..Default::default()
                },
            ],
        };
        let monitors = raw.into_monitors();
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].screen, 0);
        assert_eq!(monitors[0].plugin_kind, PluginKind::ThirdParty);
    }

    #[test]
    fn rejection_descriptions_are_all_human_readable() {
        for rejection in [
            Rejection::NoImageConfigured,
            Rejection::UnsupportedPlugin { plugin: "p".into() },
            Rejection::NotALocalFile { value: "v".into() },
            Rejection::MissingFile {
                path: PathBuf::from("/x"),
            },
            Rejection::UnreadableFile {
                path: PathBuf::from("/x"),
                message: "m".into(),
            },
        ] {
            assert!(!rejection.describe().is_empty());
        }
    }
}
