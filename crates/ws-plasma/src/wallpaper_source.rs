//! Reading the active wallpaper from Plasma 6.
//!
//! Two strategies, tried in order:
//!
//! 1. **Live query via D-Bus.** `qdbus6 org.kde.plasmashell /PlasmaShell
//!    org.kde.PlasmaShell.evaluateScript` runs Plasma's own scripting API, which
//!    reports the *current* state including plugins that resolve their image at
//!    runtime (slideshows, `a2n.blur`-style dynamic wallpapers). The prototype
//!    used this and it is the only correct source for such plugins.
//! 2. **Configuration file.** `plasma-org.kde.plasma.desktop-appletsrc` holds
//!    the same data with more latency. Used when `qdbus6` is missing or Plasma is
//!    not reachable, which is common in restricted sessions.
//!
//! The configuration parser is written here rather than pulled from a crate
//! because it must understand Plasma's nested group names
//! (`[Containments][73][Wallpaper][a2n.blur][General]`) and must not rewrite the
//! file it reads.

use std::collections::BTreeMap;
use std::path::Path;

use ws_core::wallpaper::{plugin_kind, MonitorWallpaper, RawContainer, RawWallpaperConfig};

use crate::env::Runner;

/// Plasma script that reports every desktop container's wallpaper.
///
/// Kept identical to what `plasmashell` accepts interactively. `print()` is the
/// API's output channel and the result is a JSON array.
const PLASMA_SCRIPT: &str = r#"print(JSON.stringify(desktops().map(function(d){
  d.currentConfigGroup = ["Wallpaper", d.wallpaperPlugin, "General"];
  return {screen: d.screen, plugin: String(d.wallpaperPlugin), image: String(d.readConfig("Image", ""))};
})))"#;

#[derive(Debug, thiserror::Error)]
pub enum WallpaperReadError {
    #[error("Plasma did not return a usable reply: {0}")]
    BadReply(String),
    #[error("no wallpaper source is available: {0}")]
    NoSource(String),
    #[error("cannot read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Read wallpapers through Plasma's scripting D-Bus interface.
pub fn read_live(
    runner: &dyn Runner,
    qdbus: &str,
) -> Result<Vec<MonitorWallpaper>, WallpaperReadError> {
    let output = runner
        .run(
            qdbus,
            &[
                "org.kde.plasmashell",
                "/PlasmaShell",
                "org.kde.PlasmaShell.evaluateScript",
                PLASMA_SCRIPT,
            ],
        )
        .map_err(WallpaperReadError::BadReply)?;

    parse_plasma_reply(&output)
}

/// Parse the JSON array `print()` produced.
///
/// Plasma prints exactly what was passed to `print()`, sometimes with a
/// trailing newline or a leading line from the script engine.
pub fn parse_plasma_reply(output: &str) -> Result<Vec<MonitorWallpaper>, WallpaperReadError> {
    let trimmed = output.trim();
    let start = trimmed.find('[').ok_or_else(|| {
        WallpaperReadError::BadReply(format!("no JSON array in Plasma reply: {trimmed:?}"))
    })?;
    let end = trimmed.rfind(']').ok_or_else(|| {
        WallpaperReadError::BadReply(format!(
            "unterminated JSON array in Plasma reply: {trimmed:?}"
        ))
    })?;
    let slice = &trimmed[start..=end];

    #[derive(serde::Deserialize)]
    struct Entry {
        screen: i64,
        plugin: String,
        #[serde(default)]
        image: String,
    }

    let entries: Vec<Entry> = serde_json::from_str(slice).map_err(|e| {
        WallpaperReadError::BadReply(format!("cannot parse Plasma reply {slice:?}: {e}"))
    })?;

    Ok(entries
        .into_iter()
        .filter(|entry| entry.screen >= 0)
        .map(|entry| MonitorWallpaper {
            screen: entry.screen as u32,
            plugin_kind: plugin_kind(&entry.plugin),
            plugin: entry.plugin,
            image: (!entry.image.trim().is_empty()).then_some(entry.image),
        })
        .collect())
}

/// Read wallpapers from the Plasma configuration file.
pub fn read_config(path: &Path) -> Result<Vec<MonitorWallpaper>, WallpaperReadError> {
    let text = std::fs::read_to_string(path).map_err(|source| WallpaperReadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parse_appletsrc(&text))
}

/// Parse `plasma-org.kde.plasma.desktop-appletsrc`.
///
/// Only top-level desktop containments are considered. Plasma nests every key
/// under bracketed groups, so a key's full path is the concatenation of its
/// enclosing headers, e.g.
/// `Containments / 73 / Wallpaper / a2n.blur / General`.
pub fn parse_appletsrc(text: &str) -> Vec<MonitorWallpaper> {
    /// One containment, keyed by id.
    #[derive(Default)]
    struct Containment {
        screen: Option<u32>,
        plugin: Option<String>,
        wallpaper_plugin: Option<String>,
        general: BTreeMap<String, String>,
    }

    let mut containments: BTreeMap<String, Containment> = BTreeMap::new();
    let mut current: Vec<String> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = header
                .split("][")
                .map(|part| part.trim_matches(|c| c == '[' || c == ']').to_owned())
                .filter(|part| !part.is_empty())
                .collect();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());

        // Only direct children of `[Containments][<id>]` so far.
        let Some(first) = current.first() else {
            continue;
        };
        if first != "Containments" || current.len() < 2 {
            continue;
        }
        let id = current[1].clone();
        let entry = containments.entry(id).or_default();

        match current.as_slice() {
            [_, _] => match key {
                "lastScreen" => entry.screen = value.parse().ok().filter(|s: &u32| *s < 1000),
                "wallpaperplugin" => entry.wallpaper_plugin = Some(value.to_owned()),
                "plugin" => entry.plugin = Some(value.to_owned()),
                _ => {}
            },
            [_, _, third, fourth, fifth] if third == "Wallpaper" && fifth == "General" => {
                entry.wallpaper_plugin.get_or_insert_with(|| fourth.clone());
                entry.general.insert(key.to_owned(), value.to_owned());
            }
            _ => {}
        }
    }

    // A desktop containment is one whose widget plugin is the folder view (or
    // unset) and which reports a screen. Panels have `formfactor=2` and no
    // wallpaper of their own.
    let containers = containments
        .into_iter()
        .filter(|(_, c)| {
            c.screen.is_some()
                && c.plugin
                    .as_deref()
                    .is_none_or(|p| p == "org.kde.plasma.folder")
                && c.wallpaper_plugin.is_some()
        })
        .map(|(id, c)| RawContainer {
            container_id: id,
            screen: c.screen,
            plugin: c.wallpaper_plugin,
            general: c.general,
        })
        .collect();

    // Containers are keyed by Plasma's numeric ids, so a string sort would put
    // "110" before "73". Sort by screen so callers see display order.
    let mut monitors = RawWallpaperConfig { containers }.into_monitors();
    monitors.sort_by_key(|m| m.screen);
    monitors
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

    /// Trimmed from the real file on a two-monitor Plasma 6 desktop, including
    /// a panel containment that must be ignored.
    const APPLETSRC: &str = r#"
[ActionPlugins][0]
RightButton;NoModifier=org.kde.contextmenu

[Containments][110]
activityId=350b19c9
formfactor=0
lastScreen=1
location=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][73]
activityId=350b19c9
formfactor=0
lastScreen=0
location=0
plugin=org.kde.plasma.folder
wallpaperplugin=a2n.blur

[Containments][73][Wallpaper][a2n.blur][General]
ActiveColorColor=#000000
DynamicMode=2
Image=file:///home/someone/Pictures/wallpaper.jpg
PreviewImage=file:///home/someone/Pictures/wallpaper.jpg
SlidePaths=/usr/share/wallpapers/

[Containments][110][Wallpaper][org.kde.image][General]
Image=file:///home/someone/Pictures/second.png

[Containments][74]
activityId=
formfactor=2
lastScreen=0
location=3
plugin=org.kde.panel
wallpaperplugin=org.kde.image

[ScreenMapping]
"#;

    #[test]
    fn both_desktops_are_read_and_panels_ignored() {
        let monitors = parse_appletsrc(APPLETSRC);
        assert_eq!(monitors.len(), 2, "{monitors:?}");
        assert_eq!(monitors[0].screen, 0);
        assert_eq!(monitors[0].plugin, "a2n.blur");
        assert_eq!(
            monitors[0].image.as_deref(),
            Some("file:///home/someone/Pictures/wallpaper.jpg")
        );
        assert_eq!(monitors[1].screen, 1);
        assert_eq!(monitors[1].plugin, "org.kde.image");
        // The panel (container 74) must not appear.
        assert!(monitors.iter().all(|m| m.plugin != "org.kde.panel"));
    }

    #[test]
    fn a_third_party_plugin_is_classified_as_third_party() {
        let monitors = parse_appletsrc(APPLETSRC);
        assert_eq!(monitors[0].plugin_kind, ws_core::PluginKind::ThirdParty);
    }

    #[test]
    fn keys_do_not_leak_between_containers() {
        let text = "\
[Containments][1]
lastScreen=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image
[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///a.jpg
[Containments][2]
lastScreen=1
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image
[Containments][2][Wallpaper][org.kde.image][General]
Image=file:///b.jpg
";
        let monitors = parse_appletsrc(text);
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].image.as_deref(), Some("file:///a.jpg"));
        assert_eq!(monitors[1].image.as_deref(), Some("file:///b.jpg"));
    }

    #[test]
    fn a_missing_file_section_still_reports_the_monitor() {
        let text = "\
[Containments][1]
lastScreen=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image
";
        let monitors = parse_appletsrc(text);
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].image, None, "no image is configured");
    }

    #[test]
    fn malformed_and_empty_configs_do_not_panic() {
        for text in [
            "",
            "   ",
            "garbage",
            "[Containments]",
            "[Containments][1]",
            "=x\n[]",
        ] {
            let monitors = parse_appletsrc(text);
            assert!(monitors.is_empty(), "{text:?} produced {monitors:?}");
        }
    }

    #[test]
    fn a_plasma_reply_is_parsed() {
        let reply = r#"[{"screen":0,"plugin":"a2n.blur","image":"file:///home/x/a.jpg"},{"screen":1,"plugin":"org.kde.image","image":"file:///home/x/b.png"}]"#;
        let monitors = parse_plasma_reply(reply).unwrap();
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].screen, 0);
        assert_eq!(monitors[0].plugin_kind, ws_core::PluginKind::ThirdParty);
        assert_eq!(monitors[1].plugin_kind, ws_core::PluginKind::Image);
    }

    #[test]
    fn surrounding_noise_in_the_reply_is_tolerated() {
        let reply =
            "  \n[{\"screen\":0,\"plugin\":\"org.kde.image\",\"image\":\"file:///a.jpg\"}]\n  ";
        assert!(parse_plasma_reply(reply).is_ok());
    }

    #[test]
    fn an_empty_wallpaper_value_becomes_none() {
        let reply = r#"[{"screen":0,"plugin":"org.kde.image","image":""}]"#;
        let monitors = parse_plasma_reply(reply).unwrap();
        assert_eq!(monitors[0].image, None);
    }

    #[test]
    fn a_non_json_reply_is_reported_not_guessed() {
        assert!(matches!(
            parse_plasma_reply("error: no plasma"),
            Err(WallpaperReadError::BadReply(_))
        ));
        assert!(matches!(
            parse_plasma_reply("[{bad json}]"),
            Err(WallpaperReadError::BadReply(_))
        ));
        assert!(matches!(parse_plasma_reply("[]"), Ok(v) if v.is_empty()));
    }

    #[test]
    fn live_reading_uses_qdbus_with_the_scripting_interface() {
        let runner = RecordingRunner::with_programs(&["qdbus6"]);
        runner.responses.borrow_mut().push((
            "qdbus6".into(),
            Ok(r#"[{"screen":0,"plugin":"org.kde.image","image":"file:///a.jpg"}]"#.into()),
        ));
        let monitors = read_live(&runner, "qdbus6").unwrap();
        assert_eq!(monitors.len(), 1);
        let calls = runner.calls();
        assert_eq!(calls[0].0, "qdbus6");
        assert_eq!(calls[0].1[0], "org.kde.plasmashell");
        assert_eq!(calls[0].1[2], "org.kde.PlasmaShell.evaluateScript");
    }

    #[test]
    fn a_failing_live_query_is_an_error_so_the_caller_can_fall_back() {
        let runner = RecordingRunner::with_programs(&["qdbus6"]);
        // No recorded response means an error.
        assert!(read_live(&runner, "qdbus6").is_err());
    }

    #[test]
    fn negative_screens_from_plasma_are_discarded() {
        let reply = r#"[{"screen":-1,"plugin":"org.kde.image","image":"file:///x.jpg"}]"#;
        assert!(parse_plasma_reply(reply).unwrap().is_empty());
    }
}
