//! Kvantum theme generation.
//!
//! Kvantum renders Qt widgets from a theme directory containing:
//!
//! * `<Name>.kvconfig` — an INI file of colours, metrics and behaviour flags;
//! * `<Name>.svg` — the widget artwork, with colours baked into shapes.
//!
//! There is no API for generating a theme, so the only reliable approach is what
//! the prototype did: take an existing theme the user already likes and
//! *recolour* it. That preserves the base theme's geometry and artwork quality,
//! and the product only changes the colours.
//!
//! ## What this module protects against
//!
//! * **Never writing outside our own directories.** Generated themes go to
//!   `$XDG_CONFIG_HOME/Kvantum/<generated name>`; the base theme is only read.
//! * **Never mutating a themed SVG structurally.** Only complete `#rrggbb`
//!   tokens are substituted, so an attribute value is replaced wholesale or not
//!   at all. A partial substitution would corrupt arbitrary SVG.
//! * **Losing the user's original base theme.** The base theme name is recorded
//!   once, the first time we ever generate, so a second generation still derives
//!   from the user's choice rather than from our own output.
//! * **Clobbering per-application mappings.** Only the applications we added are
//!   removed from `[Applications]`; any other mapping is preserved verbatim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ws_palette::{Palette, Variant};

/// Recorder of the base theme we were built from.
///
/// Written once and never overwritten while the base theme still exists, so
/// repeated applies converge instead of drifting towards our own colours.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BaseThemeRecord {
    pub theme: String,
}

#[derive(Debug, thiserror::Error)]
pub enum KvantumError {
    #[error("Kvantum is not configured: {path} does not exist")]
    NotConfigured { path: PathBuf },
    #[error("{path} cannot be read: {source}")]
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
    #[error("base theme {theme:?} was not found in any of: {searched}")]
    BaseThemeMissing { theme: String, searched: String },
    #[error("base theme {theme:?} is incomplete: {missing}")]
    IncompleteBaseTheme { theme: String, missing: String },
    #[error("{path} is not a valid Kvantum config: {message}")]
    Parse { path: PathBuf, message: String },
}

/// A located base theme on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseTheme {
    pub name: String,
    pub directory: PathBuf,
}

impl BaseTheme {
    pub fn kvconfig(&self) -> PathBuf {
        self.directory.join(format!("{}.kvconfig", self.name))
    }

    pub fn svg(&self) -> PathBuf {
        self.directory.join(format!("{}.svg", self.name))
    }
}

/// Find a base theme by name under the user then system Kvantum roots.
///
/// User themes win, matching Kvantum's own lookup order.
pub fn locate_base_theme(
    name: &str,
    user_root: &Path,
    system_roots: &[PathBuf],
) -> Option<BaseTheme> {
    if name.trim().is_empty() {
        return None;
    }
    // A theme name is a single path segment; refuse anything that would escape
    // the theme root.
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        log::warn!("ignoring suspicious Kvantum theme name {name:?}");
        return None;
    }
    let candidates = std::iter::once(user_root.to_path_buf())
        .chain(system_roots.iter().cloned())
        .map(|root| root.join(name));
    for directory in candidates {
        let theme = BaseTheme {
            name: name.to_owned(),
            directory: directory.clone(),
        };
        if theme.kvconfig().is_file() && theme.svg().is_file() {
            return Some(theme);
        }
    }
    None
}

/// The generated themes for one palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedThemes {
    /// The opaque theme, always produced.
    pub opaque: GeneratedTheme,
    /// The translucent theme, produced when requested and the base supports it.
    pub glass: Option<GeneratedTheme>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTheme {
    pub name: String,
    pub kvconfig: String,
    pub svg: String,
    /// True when the theme asks Kvantum for window and menu translucency.
    pub translucent: bool,
}

/// Build a theme name that changes with the palette, like the colour scheme
/// name, so Kvantum reloads instead of serving a cached theme.
pub fn theme_name(palette: &Palette, variant: Variant, glass: bool) -> String {
    let accent = palette.accent;
    let suffix = if glass { "Glass" } else { "" };
    format!(
        "WallpaperStyle{}{}{:02x}{:02x}{:02x}",
        variant_label(variant),
        suffix,
        accent.r,
        accent.g,
        accent.b
    )
}

fn variant_label(variant: Variant) -> &'static str {
    match variant {
        Variant::Dark => "Dark",
        Variant::Light => "Light",
    }
}

/// Generate recoloured themes from a base theme.
///
/// `glass` asks for the translucent variant, which is only possible when the
/// base theme exposes the Kvantum translucency keys; when it does not, the
/// option returns `None` and the caller reports that background-only
/// transparency is unavailable rather than silently producing an opaque theme
/// that claims to be transparent.
pub fn generate(
    base: &BaseTheme,
    palette: &Palette,
    variant: Variant,
    want_glass: bool,
) -> Result<GeneratedThemes, KvantumError> {
    let base_config =
        std::fs::read_to_string(base.kvconfig()).map_err(|source| KvantumError::Read {
            path: base.kvconfig(),
            source,
        })?;
    let base_svg = std::fs::read_to_string(base.svg()).map_err(|source| KvantumError::Read {
        path: base.svg(),
        source,
    })?;

    let opaque_name = theme_name(palette, variant, false);
    let opaque = recolour(
        &base_config,
        &base_svg,
        palette,
        variant,
        &opaque_name,
        false,
    );

    let glass = if want_glass {
        let glass_name = theme_name(palette, variant, true);
        let generated = recolour(&base_config, &base_svg, palette, variant, &glass_name, true);
        // Only advertise the glass theme when translucency can actually be
        // requested; otherwise it is an opaque duplicate.
        generated.translucent.then_some(generated)
    } else {
        None
    };

    Ok(GeneratedThemes { opaque, glass })
}

/// Replace every complete `#rrggbb` token, case-insensitively.
///
/// Partial matches are impossible by construction: only whole six-digit tokens
/// with a `#` before them are considered, and the match must end at a
/// non-hex-digit character. That prevents turning `#3ddc972` into garbage.
pub fn replace_hex_tokens(text: &str, replacements: &[(String, String)]) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 7 <= bytes.len() {
            let candidate = &text[i + 1..i + 7];
            let is_hex = candidate.bytes().all(|b| b.is_ascii_hexdigit());
            let ends_cleanly = i + 7 == bytes.len() || !bytes[i + 7].is_ascii_hexdigit();
            if is_hex && ends_cleanly {
                let lower = format!("#{}", candidate.to_ascii_lowercase());
                match replacements.iter().find(|(from, _)| *from == lower) {
                    Some((_, to)) => out.push_str(to),
                    None => out.push_str(&text[i..i + 7]),
                }
                i += 7;
                continue;
            }
        }
        // Copy one UTF-8 character, not one byte.
        let ch = text[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Build the substitution table between the base theme's colours and ours.
///
/// The base theme's own `highlight.color` is discovered from its config rather
/// than assumed, so a theme that does not use the prototype's mint accent is
/// recoloured correctly.
pub fn substitutions(base_config: &str, palette: &Palette) -> Vec<(String, String)> {
    let base_highlight = base_config
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "highlight.color").then(|| value.trim().to_owned())
        })
        .unwrap_or_else(|| "#3ddc97".to_owned());

    let mut map: BTreeMap<String, String> = BTreeMap::new();
    // Order matters for the baseline: the discovered highlight and the common
    // defaults are all mapped to the accent.
    for accent_like in [
        base_highlight.to_ascii_lowercase(),
        "#3ddc97".to_owned(),
        "#89b4fa".to_owned(),
    ] {
        map.insert(accent_like, palette.accent.to_hex().to_lowercase());
    }
    // Known dark-theme surface tones from common Kvantum bases (Catppuccin and
    // Breeze derivatives). These are the ones that visibly clash if left alone.
    for (from, to) in [
        ("#111418", palette.background.to_hex()),
        ("#1b2127", palette.surface.to_hex()),
        ("#181825", palette.surface.to_hex()),
        ("#1e1e2e", palette.surface.to_hex()),
        ("#313244", palette.button.to_hex()),
        ("#232634", palette.background.to_hex()),
    ] {
        map.insert(from.to_owned(), to.to_lowercase());
    }
    map.into_iter().collect()
}

fn recolour(
    base_config: &str,
    base_svg: &str,
    palette: &Palette,
    variant: Variant,
    name: &str,
    glass: bool,
) -> GeneratedTheme {
    let table = substitutions(base_config, palette);
    let mut config = replace_hex_tokens(base_config, &table);
    let svg = replace_hex_tokens(base_svg, &table);

    config = set_general_colors(&config, palette, variant);
    let translucent = if glass {
        apply_translucency(&mut config)
    } else {
        false
    };

    GeneratedTheme {
        name: name.to_owned(),
        kvconfig: config,
        svg,
        translucent,
    }
}

/// Rewrite the `[GeneralColors]` block from the palette. These keys are what
/// Kvantum actually uses for widget backgrounds, so they must be exact rather
/// than whatever a hex substitution happened to produce.
fn set_general_colors(config: &str, palette: &Palette, variant: Variant) -> String {
    let mut doc = IniDoc::parse(config);
    doc.set("GeneralColors", "window.color", &palette.surface.to_hex());
    doc.set("GeneralColors", "base.color", &palette.background.to_hex());
    doc.set(
        "GeneralColors",
        "alt.base.color",
        &palette.background.to_hex(),
    );
    doc.set("GeneralColors", "button.color", &palette.button.to_hex());
    doc.set("GeneralColors", "dark.color", &palette.background.to_hex());
    doc.set("GeneralColors", "mid.color", &palette.background.to_hex());
    doc.set("GeneralColors", "light.color", &palette.hover.to_hex());
    doc.set("GeneralColors", "mid.light.color", &palette.hover.to_hex());
    doc.set("GeneralColors", "highlight.color", &palette.accent.to_hex());
    doc.set(
        "GeneralColors",
        "inactive.highlight.color",
        &palette.accent.to_hex(),
    );
    doc.set("GeneralColors", "text.color", &palette.text.to_hex());
    doc.set("GeneralColors", "window.text.color", &palette.text.to_hex());
    doc.set("GeneralColors", "button.text.color", &palette.text.to_hex());
    doc.set(
        "GeneralColors",
        "tooltip.text.color",
        &palette.text.to_hex(),
    );
    doc.set(
        "GeneralColors",
        "highlight.text.color",
        &palette.accent_text.to_hex(),
    );
    doc.set("GeneralColors", "link.color", &palette.info.to_hex());
    doc.set(
        "GeneralColors",
        "link.visited.color",
        &palette.info.to_hex(),
    );
    doc.set(
        "GeneralColors",
        "disabled.text.color",
        &palette.text_muted.to_hex(),
    );

    // Kvantum's own light/dark preference, so widget details shade correctly.
    doc.set(
        "Hacks",
        "respect_darkness",
        if matches!(variant, Variant::Dark) {
            "true"
        } else {
            "false"
        },
    );

    // Selection text inside item views is separate from the global one.
    doc.set(
        "ItemView",
        "text.toggle.color",
        &palette.accent_text.to_hex(),
    );
    doc.set(
        "PanelButtonCommand",
        "text.press.color",
        &palette.accent_text.to_hex(),
    );
    doc.set(
        "PanelButtonCommand",
        "text.toggle.color",
        &palette.accent_text.to_hex(),
    );
    doc.set("Menu", "text.press.color", &palette.accent_text.to_hex());
    doc.set("Menu", "text.toggle.color", &palette.accent_text.to_hex());
    doc.set("Tab", "text.focus.color", &palette.text.to_hex());
    doc.set(
        "HeaderSection",
        "text.focus.color",
        &palette.accent.to_hex(),
    );
    doc.set("Toolbar", "text.press.color", &palette.accent.to_hex());
    doc.set("Toolbar", "text.toggle.color", &palette.accent.to_hex());

    doc.render()
}

/// Turn on Kvantum's translucency. Returns whether the theme now requests it.
///
/// Kvantum itself decides whether translucency is possible: on a compositor with
/// blur support the surfaces become translucent, otherwise it falls back to
/// opaque. The keys are what make the request.
fn apply_translucency(config: &mut String) -> bool {
    let mut doc = IniDoc::parse(config);
    doc.set("%General", "translucent_windows", "true");
    doc.set("%General", "blurring", "true");
    doc.set("%General", "popup_blurring", "true");
    // A gentle reduction: enough to see the wallpaper, not enough to hurt text,
    // which stays fully opaque.
    doc.set("%General", "reduce_window_opacity", "20");
    doc.set("%General", "reduce_menu_opacity", "20");
    doc.set("Hacks", "blur_konsole", "true");
    doc.set("Hacks", "blur_translucent", "true");
    *config = doc.render();
    // Report honestly: the base theme must understand these keys.
    let check = IniDoc::parse(config);
    check.get("%General", "translucent_windows").as_deref() == Some("true")
}

/// A minimal INI editor that preserves comments, ordering and `%`-prefixed
/// groups (Kvantum uses `[%General]`).
#[derive(Debug, Clone)]
struct IniDoc {
    lines: Vec<String>,
}

impl IniDoc {
    fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
        }
    }

    fn render(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    fn bounds(&self, group: &str) -> Option<(usize, usize)> {
        let header = format!("[{group}]");
        let start = self.lines.iter().position(|l| l.trim() == header)?;
        let end = self.lines[start + 1..]
            .iter()
            .position(|l| {
                let t = l.trim();
                t.starts_with('[') && t.ends_with(']')
            })
            .map_or(self.lines.len(), |o| start + 1 + o);
        Some((start, end))
    }

    fn get(&self, group: &str, key: &str) -> Option<String> {
        let (start, end) = self.bounds(group)?;
        self.lines[start + 1..end].iter().find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_owned())
        })
    }

    fn set(&mut self, group: &str, key: &str, value: &str) -> bool {
        let line = format!("{key}={value}");
        match self.bounds(group) {
            Some((start, end)) => {
                for existing in &mut self.lines[start + 1..end] {
                    if let Some((k, _)) = existing.split_once('=') {
                        if k.trim() == key {
                            if *existing == line {
                                return false;
                            }
                            *existing = line;
                            return true;
                        }
                    }
                }
                self.lines.insert(end, line);
                true
            }
            None => {
                if self.lines.last().is_some_and(|l| !l.trim().is_empty()) {
                    self.lines.push(String::new());
                }
                self.lines.push(format!("[{group}]"));
                self.lines.push(line);
                true
            }
        }
    }
}

/// Update `kvantum.kvconfig` so the generated theme is active and the listed
/// applications use the glass variant.
///
/// Only mappings naming one of *our* themes are removed; a user's own per-app
/// mapping is preserved. Returns the new file contents.
pub fn update_kvconfig(
    text: &str,
    active_theme: &str,
    glass_theme: Option<&str>,
    glass_apps: &[String],
) -> String {
    let mut doc = IniDoc::parse(text);
    doc.set("General", "theme", active_theme);

    // Delete every mapping naming one of *our* themes outright, then re-add the
    // current one below. Dropping only the apps listed in `glass_apps` left the
    // others stranded in a mapping pointing at a theme that no longer exists,
    // which made Kwantum fall back to its default for those applications.
    if let Some((start, end)) = doc.bounds("Applications") {
        let mut to_remove = Vec::new();
        for index in start + 1..end.min(doc.lines.len()) {
            let line = doc.lines[index].clone();
            if let Some((class, _)) = line.split_once('=') {
                if class.trim().starts_with("WallpaperStyle") {
                    to_remove.push(index);
                }
            }
        }
        for index in to_remove.into_iter().rev() {
            doc.lines.remove(index);
        }
    }

    if let Some(glass) = glass_theme {
        if !glass_apps.is_empty() {
            let apps: Vec<&str> = glass_apps
                .iter()
                .map(String::as_str)
                .filter(|a| !a.is_empty())
                .collect();
            doc.set("Applications", glass, &apps.join(", "));
        }
    }
    doc.render()
}

/// Where a generated theme lives.
pub fn theme_dir(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

/// Names of themes this application generated under a Kvantum root.
pub fn generated_theme_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("WallpaperStyle"))
        })
        .collect()
}

/// Read the base-theme record, falling back to whatever the config currently
/// names.
pub fn base_theme_name(record_path: &Path, kvconfig_text: &str) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(record_path) {
        if let Ok(record) = serde_json::from_str::<BaseThemeRecord>(&text) {
            if !record.theme.trim().is_empty() {
                return Some(record.theme);
            }
        }
    }
    let doc = IniDoc::parse(kvconfig_text);
    doc.get("General", "theme").filter(|t| !t.trim().is_empty())
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
    use ws_palette::{build, Rgb};

    const BASE_CONFIG: &str = "\
[%General]
theme_borders=false
translucent_windows=false
blurring=false

[GeneralColors]
window.color=#1b2127
base.color=#111418
highlight.color=#3ddc97
text.color=#e8edf3

[ItemView]
inherits=PanelButtonCommand
text.toggle.color=#111418

[Applications]
SomeOtherTheme=dolphin
";

    fn palette(variant: Variant) -> Palette {
        build(Rgb::from_hex("#5f6fdd").unwrap(), variant, 255).palette
    }

    fn base_theme(dir: &Path) -> BaseTheme {
        std::fs::create_dir_all(dir).unwrap();
        let name = "Base";
        std::fs::write(dir.join("Base.kvconfig"), BASE_CONFIG).unwrap();
        std::fs::write(
            dir.join("Base.svg"),
            r##"<svg><rect fill="#3ddc97"/><circle stroke="#111418" fill="#1b2127"/></svg>"##,
        )
        .unwrap();
        BaseTheme {
            name: name.into(),
            directory: dir.to_path_buf(),
        }
    }

    #[test]
    fn hex_tokens_are_replaced_completely() {
        let out = replace_hex_tokens(
            r##"<a fill="#3Ddc97" stroke="#111418"/>"##,
            &[
                ("#3ddc97".into(), "#ff0000".into()),
                ("#111418".into(), "#00ff00".into()),
            ],
        );
        assert!(out.contains(r##"fill="#ff0000""##), "{out}");
        assert!(out.contains(r##"stroke="#00ff00""##), "{out}");
    }

    #[test]
    fn partial_hex_matches_are_left_alone() {
        // Seven hex digits: not a complete colour token.
        let input = "<a fill=\"#3ddc972\"/>";
        let out = replace_hex_tokens(input, &[("#3ddc97".into(), "#ff0000".into())]);
        assert_eq!(
            out, input,
            "a 7-digit value must not be partially rewritten"
        );
    }

    #[test]
    fn replacement_does_not_touch_similar_prefixes() {
        let out = replace_hex_tokens(
            "#1114180 and #11141",
            &[("#111418".into(), "#aaaaaa".into())],
        );
        assert_eq!(out, "#1114180 and #11141");
    }

    #[test]
    fn non_ascii_content_survives_replacement() {
        let out = replace_hex_tokens(
            "ünïcode ☃ #3ddc97 naïve",
            &[("#3ddc97".into(), "#123456".into())],
        );
        assert_eq!(out, "ünïcode ☃ #123456 naïve");
    }

    #[test]
    fn general_colors_follow_the_palette() {
        let p = palette(Variant::Dark);
        let generated = recolour(BASE_CONFIG, "<svg/>", &p, Variant::Dark, "T", false);
        let doc = IniDoc::parse(&generated.kvconfig);
        assert_eq!(
            doc.get("GeneralColors", "highlight.color").unwrap(),
            p.accent.to_hex()
        );
        assert_eq!(
            doc.get("GeneralColors", "text.color").unwrap(),
            p.text.to_hex()
        );
        assert_eq!(
            doc.get("GeneralColors", "window.color").unwrap(),
            p.surface.to_hex()
        );
        assert_eq!(
            doc.get("GeneralColors", "highlight.text.color").unwrap(),
            p.accent_text.to_hex()
        );
    }

    #[test]
    fn glass_variant_requests_translucency() {
        let p = palette(Variant::Dark);
        let generated = recolour(BASE_CONFIG, "<svg/>", &p, Variant::Dark, "TGlass", true);
        assert!(generated.translucent);
        let doc = IniDoc::parse(&generated.kvconfig);
        assert_eq!(
            doc.get("%General", "translucent_windows").as_deref(),
            Some("true")
        );
        assert_eq!(doc.get("%General", "blurring").as_deref(), Some("true"));
    }

    #[test]
    fn opaque_variant_never_enables_translucency() {
        let p = palette(Variant::Dark);
        let generated = recolour(BASE_CONFIG, "<svg/>", &p, Variant::Dark, "T", false);
        assert!(!generated.translucent);
        let doc = IniDoc::parse(&generated.kvconfig);
        assert_ne!(
            doc.get("%General", "translucent_windows").as_deref(),
            Some("true")
        );
    }

    #[test]
    fn comments_and_unrelated_settings_survive_generation() {
        let with_comment = format!("# keep me\n{BASE_CONFIG}");
        let p = palette(Variant::Dark);
        let generated = recolour(&with_comment, "<svg/>", &p, Variant::Dark, "T", false);
        assert!(
            generated.kvconfig.contains("# keep me"),
            "{}",
            generated.kvconfig
        );
        let doc = IniDoc::parse(&generated.kvconfig);
        assert_eq!(
            doc.get("%General", "theme_borders").as_deref(),
            Some("false")
        );
    }

    #[test]
    fn theme_names_are_palette_specific_and_unique() {
        let dark = palette(Variant::Dark);
        let light = palette(Variant::Light);
        assert_ne!(
            theme_name(&dark, Variant::Dark, false),
            theme_name(&light, Variant::Light, false)
        );
        assert!(theme_name(&dark, Variant::Dark, true).contains("Glass"));
        assert!(theme_name(&dark, Variant::Dark, false).starts_with("WallpaperStyle"));
    }

    #[test]
    fn generation_from_disk_is_reproducible() {
        let dir = tempfile::tempdir().unwrap();
        let base = base_theme(dir.path());
        let p = palette(Variant::Dark);
        let first = generate(&base, &p, Variant::Dark, true).unwrap();
        let second = generate(&base, &p, Variant::Dark, true).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn the_base_theme_is_never_written_to() {
        let dir = tempfile::tempdir().unwrap();
        let base = base_theme(dir.path());
        let before = std::fs::read_to_string(base.kvconfig()).unwrap();
        let p = palette(Variant::Dark);
        generate(&base, &p, Variant::Dark, true).unwrap();
        let after = std::fs::read_to_string(base.kvconfig()).unwrap();
        assert_eq!(before, after, "generation must not modify the base theme");
    }

    #[test]
    fn a_missing_base_theme_is_reported_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let base = BaseTheme {
            name: "Nope".into(),
            directory: dir.path().join("Nope"),
        };
        let p = palette(Variant::Dark);
        let err = generate(&base, &p, Variant::Dark, false).unwrap_err();
        assert!(matches!(err, KvantumError::Read { .. }), "{err}");
    }

    #[test]
    fn base_theme_lookup_refuses_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["../etc/passwd", "..", ".", "a/b"] {
            assert!(locate_base_theme(name, dir.path(), &[]).is_none(), "{name}");
        }
    }

    #[test]
    fn base_theme_lookup_prefers_the_user_root() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        for root in [user.path(), system.path()] {
            std::fs::create_dir_all(root.join("Shared")).unwrap();
            std::fs::write(root.join("Shared/Shared.kvconfig"), "[%General]\n").unwrap();
            std::fs::write(root.join("Shared/Shared.svg"), "<svg/>").unwrap();
        }
        let found =
            locate_base_theme("Shared", user.path(), &[system.path().to_path_buf()]).unwrap();
        assert_eq!(found.directory, user.path().join("Shared"));
    }

    #[test]
    fn base_theme_lookup_requires_both_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Half")).unwrap();
        std::fs::write(dir.path().join("Half/Half.kvconfig"), "[%General]\n").unwrap();
        // No SVG: incomplete.
        assert!(locate_base_theme("Half", dir.path(), &[]).is_none());
    }

    #[test]
    fn kvconfig_activation_sets_the_theme_and_keeps_foreign_mappings() {
        let text = "[General]\ntheme=OldBase\n\n[Applications]\nSomeOther=dolphin, kate\n";
        let out = update_kvconfig(
            text,
            "WallpaperStyleDark5f6fdd",
            Some("WallpaperStyleDarkGlass5f6fdd"),
            &["dolphin".into()],
        );
        let doc = IniDoc::parse(&out);
        assert_eq!(
            doc.get("General", "theme").as_deref(),
            Some("WallpaperStyleDark5f6fdd")
        );
        assert_eq!(
            doc.get("Applications", "SomeOther").as_deref(),
            Some("dolphin, kate")
        );
        assert_eq!(
            doc.get("Applications", "WallpaperStyleDarkGlass5f6fdd")
                .as_deref(),
            Some("dolphin")
        );
    }

    #[test]
    fn kvconfig_is_idempotent_across_repeated_applies() {
        let text = "[General]\ntheme=OldBase\n\n[Applications]\nSomeOther=dolphin\n";
        let once = update_kvconfig(
            text,
            "WallpaperStyleDark5f6fdd",
            Some("WallpaperStyleDarkGlass5f6fdd"),
            &["dolphin".into()],
        );
        let twice = update_kvconfig(
            &once,
            "WallpaperStyleDark5f6fdd",
            Some("WallpaperStyleDarkGlass5f6fdd"),
            &["dolphin".into()],
        );
        assert_eq!(once, twice, "repeated applies must converge");
    }

    #[test]
    fn switching_off_glass_removes_only_our_mappings() {
        let text = "[General]\ntheme=OldBase\n\n[Applications]\nSomeOther=dolphin\n";
        let with_glass = update_kvconfig(
            text,
            "WallpaperStyleDark5f6fdd",
            Some("WallpaperStyleDarkGlass5f6fdd"),
            &["dolphin".into()],
        );
        let without = update_kvconfig(
            &with_glass,
            "WallpaperStyleDark5f6fdd",
            None,
            &["dolphin".into()],
        );
        let doc = IniDoc::parse(&without);
        assert_eq!(
            doc.get("Applications", "WallpaperStyleDarkGlass5f6fdd"),
            None
        );
        assert_eq!(
            doc.get("Applications", "SomeOther").as_deref(),
            Some("dolphin")
        );
    }

    #[test]
    fn our_own_old_theme_entries_do_not_accumulate() {
        let text =
            "[General]\ntheme=OldBase\n\n[Applications]\nWallpaperStyleDark5f6fdd=dolphin, kate\n";
        let out = update_kvconfig(
            text,
            "WallpaperStyleDark111111",
            Some("WallpaperStyleDarkGlass111111"),
            &["dolphin".into()],
        );
        let doc = IniDoc::parse(&out);
        assert_eq!(doc.get("Applications", "WallpaperStyleDark5f6fdd"), None);
    }

    #[test]
    fn the_base_theme_record_wins_over_the_live_config() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("base.json");
        std::fs::write(&record, r#"{"theme":"UserChoice"}"#).unwrap();
        let name = base_theme_name(&record, "[General]\ntheme=OurOwnTheme\n").unwrap();
        assert_eq!(
            name, "UserChoice",
            "a second generation must not derive from our output"
        );
    }

    #[test]
    fn without_a_record_the_live_config_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("absent.json");
        let name = base_theme_name(&record, "[General]\ntheme=Fresh\n").unwrap();
        assert_eq!(name, "Fresh");
    }

    #[test]
    fn generated_theme_discovery_ignores_user_themes() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["WallpaperStyleDark5f6fdd", "MokkaMint", "WallpaperStyle"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        let found = generated_theme_dirs(dir.path());
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|p| p
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("WallpaperStyle")));
    }

    #[test]
    fn base_highlight_is_discovered_rather_than_assumed() {
        let config = "[GeneralColors]\nhighlight.color=#ff8800\n";
        let p = palette(Variant::Dark);
        let table = substitutions(config, &p);
        assert!(
            table
                .iter()
                .any(|(from, to)| from == "#ff8800" && *to == p.accent.to_hex()),
            "{table:?}"
        );
    }

    #[test]
    fn light_variant_sets_respect_darkness_off() {
        let p = palette(Variant::Light);
        let generated = recolour(BASE_CONFIG, "<svg/>", &p, Variant::Light, "T", false);
        let doc = IniDoc::parse(&generated.kvconfig);
        assert_eq!(
            doc.get("Hacks", "respect_darkness").as_deref(),
            Some("false")
        );
    }
}
