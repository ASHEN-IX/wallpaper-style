//! Generation of KDE Plasma colour scheme files (`.colors`).
//!
//! The format is an INI file with a `[General]` section naming the scheme and a
//! set of `[Colors:*]` sections. KDE has no schema for it, so this module is
//! built from an observed-good reference (the scheme the prototype produced and
//! Plasma accepted) and from the sections Plasma itself reads.
//!
//! Two rules matter for correctness:
//!
//! * **Every section carries every key.** Plasma falls back to hard-coded
//!   defaults for missing keys, which produces mismatched text colours in
//!   tooltips or headers. Emitting the full set avoids surprises.
//! * **`[General] Name` must be unique per palette.** Plasma caches a scheme by
//!   name and will not re-apply a scheme whose name it has already activated,
//!   even if the file changed. The prototype solved this with a colour-specific
//!   suffix and so do we.
//!
//! Colours are written as `r,g,b` triples, which is what the format requires.

use std::fmt::Write as _;

use ws_palette::{Palette, Variant};

/// The KDE colour sections that carry a background and foreground.
pub const COLOR_SECTIONS: [&str; 7] = [
    "Colors:Button",
    "Colors:Complementary",
    "Colors:Header",
    "Colors:Selection",
    "Colors:Tooltip",
    "Colors:View",
    "Colors:Window",
];

/// A generated colour scheme file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorScheme {
    /// The scheme name, used for the file name and for `[General] Name`.
    pub name: String,
    /// The complete INI text.
    pub text: String,
}

impl ColorScheme {
    /// The file name Plasma expects, `<Name>.colors`.
    pub fn file_name(&self) -> String {
        format!("{}.colors", self.name)
    }
}

/// Build a scheme name that is stable for a palette but changes when the
/// palette changes, so Plasma re-applies it.
///
/// The accent is included because it is the colour the user actually perceives;
/// two wallpapers that produce the same accent but different surfaces would
/// otherwise collide.
pub fn scheme_name(palette: &Palette, variant: Variant) -> String {
    let accent = palette.accent;
    let surface = palette.surface;
    format!(
        "Wallpaper Style {} {:02x}{:02x}{:02x}-{:02x}{:02x}{:02x}",
        variant_label(variant),
        accent.r,
        accent.g,
        accent.b,
        surface.r,
        surface.g,
        surface.b
    )
}

fn variant_label(variant: Variant) -> &'static str {
    match variant {
        Variant::Dark => "Dark",
        Variant::Light => "Light",
    }
}

/// Values for one `[Colors:*]` section.
struct SectionColors {
    background: ws_palette::Rgb,
    alternate: ws_palette::Rgb,
    foreground: ws_palette::Rgb,
    decoration: ws_palette::Rgb,
}

/// Generate a complete `.colors` file for a palette.
///
/// `inactive_is_dim` matches Plasma's own behaviour: inactive window text is
/// dimmed rather than recoloured.
pub fn generate(palette: &Palette, variant: Variant) -> ColorScheme {
    let name = scheme_name(palette, variant);

    let background = palette.background.solid();
    let mut out = String::with_capacity(4096);

    // Plasma's own schemes put the disabled/inactive effects first.
    write_effects(&mut out, palette, variant);

    for section in COLOR_SECTIONS {
        let colors = section_colors(palette, section, variant);
        write_section(&mut out, section, &colors, palette);
    }

    let _ = writeln!(out, "[General]");
    let _ = writeln!(out, "Name={name}");
    let _ = writeln!(out, "ColorScheme={}", name.replace(' ', ""));
    // Plasma 6 takes the shell's accent from this key, not from
    // `[Colors:Selection]`. `plasma-apply-colorscheme` copies it into
    // `kdeglobals` only when the scheme file contains it, so a scheme without it
    // leaves the Kickoff selection pill and the category icons on the previous
    // accent while Qt applications switch correctly.
    let _ = writeln!(out, "AccentColor={}", accent_key(palette));
    let _ = writeln!(out, "shadeSortColumn={}", matches!(variant, Variant::Dark));
    let _ = writeln!(out, "Background={}", background.to_kde_triplet());

    ColorScheme { name, text: out }
}

fn section_colors(palette: &Palette, section: &str, _variant: Variant) -> SectionColors {
    match section {
        "Colors:Window" | "Colors:Header" => SectionColors {
            background: palette.surface.solid(),
            alternate: palette.button.solid(),
            foreground: palette.text.solid(),
            decoration: palette.accent.solid(),
        },
        "Colors:Button" => SectionColors {
            background: palette.button.solid(),
            // The alternate tone is what Plasma paints behind pressed and
            // hovered buttons.
            alternate: palette.hover.solid(),
            foreground: palette.text.solid(),
            decoration: palette.accent.solid(),
        },
        "Colors:View" => SectionColors {
            background: palette.background.solid(),
            alternate: palette.surface.solid(),
            foreground: palette.text.solid(),
            decoration: palette.accent.solid(),
        },
        "Colors:Selection" => SectionColors {
            background: palette.selection.solid(),
            alternate: palette.selection.solid(),
            foreground: palette.accent_text.solid(),
            decoration: palette.accent.solid(),
        },
        "Colors:Tooltip" => SectionColors {
            background: palette.surface.solid(),
            alternate: palette.button.solid(),
            foreground: palette.text.solid(),
            decoration: palette.accent.solid(),
        },
        "Colors:Complementary" => SectionColors {
            background: palette.background.solid(),
            alternate: palette.surface.solid(),
            foreground: palette.text.solid(),
            decoration: palette.accent.solid(),
        },
        _ => SectionColors {
            background: palette.surface.solid(),
            alternate: palette.button.solid(),
            foreground: palette.text.solid(),
            decoration: palette.accent.solid(),
        },
    }
}

fn write_section(out: &mut String, section: &str, colors: &SectionColors, palette: &Palette) {
    let _ = writeln!(out, "[{section}]");

    // Selection uses the accent foreground; every other section uses body text.
    let foreground = colors.foreground.to_kde_triplet();
    let _ = writeln!(
        out,
        "BackgroundNormal={}",
        colors.background.to_kde_triplet()
    );
    let _ = writeln!(
        out,
        "BackgroundAlternate={}",
        colors.alternate.to_kde_triplet()
    );
    let _ = writeln!(out, "ForegroundNormal={foreground}");
    let _ = writeln!(
        out,
        "ForegroundInactive={}",
        palette.text_muted.solid().to_kde_triplet()
    );
    let _ = writeln!(
        out,
        "ForegroundActive={}",
        palette.accent.solid().to_kde_triplet()
    );
    let _ = writeln!(
        out,
        "ForegroundLink={}",
        palette.info.solid().to_kde_triplet()
    );
    let _ = writeln!(
        out,
        "ForegroundVisited={}",
        palette.info.solid().to_kde_triplet()
    );
    let _ = writeln!(
        out,
        "ForegroundNegative={}",
        palette.warning.solid().to_kde_triplet()
    );
    let _ = writeln!(
        out,
        "ForegroundNeutral={}",
        palette.warning.solid().to_kde_triplet()
    );
    let _ = writeln!(
        out,
        "ForegroundPositive={}",
        palette.success.solid().to_kde_triplet()
    );
    let decoration = colors.decoration.to_kde_triplet();
    let _ = writeln!(out, "DecorationFocus={decoration}");
    let _ = writeln!(out, "DecorationHover={decoration}");
}

fn write_effects(out: &mut String, palette: &Palette, variant: Variant) {
    // Disabled widgets: Plasma desaturates and dims them itself, so the colour
    // here only sets the tint direction.
    let _ = writeln!(out, "[ColorEffects:Disabled]");
    let _ = writeln!(out, "Color={}", palette.background.solid().to_kde_triplet());
    let _ = writeln!(out, "ColorAmount=0.3");
    let _ = writeln!(out, "ColorEffect=2");
    let _ = writeln!(out, "ContrastAmount=0.1");
    let _ = writeln!(out, "ContrastEffect=0");
    let _ = writeln!(out, "IntensityAmount=-1");
    let _ = writeln!(out, "IntensityEffect=0");

    let _ = writeln!(out);
    let _ = writeln!(out, "[ColorEffects:Inactive]");
    let _ = writeln!(out, "ChangeSelectionColor=true");
    let _ = writeln!(out, "Color={}", palette.background.solid().to_kde_triplet());
    // Light themes need less dimming or inactive windows look disabled.
    let amount = match variant {
        Variant::Dark => "0.5",
        Variant::Light => "0.35",
    };
    let _ = writeln!(out, "ColorAmount={amount}");
    let _ = writeln!(out, "ColorEffect=3");
    let _ = writeln!(out, "ContrastAmount=0");
    let _ = writeln!(out, "ContrastEffect=0");
    let _ = writeln!(out, "Enable=true");
    let _ = writeln!(out, "IntensityAmount=0");
    let _ = writeln!(out, "IntensityEffect=0");
    let _ = writeln!(out);
}

/// Parse a `.colors` file far enough to check it is well formed. Used by tests
/// and by the "is this scheme ours" check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedScheme {
    pub name: Option<String>,
    pub sections: Vec<String>,
    pub entries: std::collections::BTreeMap<(String, String), String>,
}

impl ParsedScheme {
    pub fn parse(text: &str) -> Self {
        let mut sections = Vec::new();
        let mut entries = std::collections::BTreeMap::new();
        let mut current = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = group.to_owned();
                sections.push(current.clone());
            } else if let Some((key, value)) = line.split_once('=') {
                entries.insert(
                    (current.clone(), key.trim().to_owned()),
                    value.trim().to_owned(),
                );
            }
        }
        let name = entries
            .get(&("General".to_owned(), "Name".to_owned()))
            .cloned();
        Self {
            name,
            sections,
            entries,
        }
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.entries
            .get(&(section.to_owned(), key.to_owned()))
            .map(String::as_str)
    }
}

/// Render a palette as a self-contained SVG specimen window.
///
/// Shapes only, no `<text>`: keeping it text-free makes the output render
/// identically everywhere (no font substitution), keeps it small, and avoids
/// embedding user data. The layout mirrors the GUI's preview panel so
/// documentation images match what users see.
pub fn preview_svg(palette: &Palette, variant: Variant) -> String {
    let hex = |c: ws_palette::Rgb| c.solid().to_hex();
    let background = hex(palette.background);
    let surface = hex(palette.surface);
    let button = hex(palette.button);
    let hover = hex(palette.hover);
    let accent = hex(palette.accent);
    let accent_text = hex(palette.accent_text);
    let text = hex(palette.text);
    let muted = hex(palette.text_muted);
    let warning = hex(palette.warning);
    let view = background.clone();
    let label = match variant {
        Variant::Dark => "Dark",
        Variant::Light => "Light",
    };

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="640" height="400" viewBox="0 0 640 400" role="img" aria-label="Wallpaper Style {label} palette preview">
  <defs>
    <!-- Everything inside the window frame is clipped to its rounded corners,
         so the sidebar cannot spill past the bottom edge. -->
    <clipPath id="windowBody">
      <rect width="640" height="400" rx="18"/>
    </clipPath>
  </defs>
  <!-- Generated by `wallpaper-style export-preview`. Shapes only, no text,
       so it renders identically everywhere. -->
  <rect width="640" height="400" rx="18" fill="{surface}"/>
  <g clip-path="url(#windowBody)">

  <!-- Title bar -->
  <rect x="0" y="0" width="640" height="42" rx="18" fill="{button}"/>
  <rect x="0" y="24" width="640" height="18" fill="{button}"/>
  <circle cx="28" cy="21" r="6" fill="{warning}"/>
  <circle cx="48" cy="21" r="6" fill="{accent}"/>
  <circle cx="68" cy="21" r="6" fill="{muted}"/>
  <rect x="92" y="16" width="120" height="10" rx="5" fill="{text}" opacity="0.55"/>

  <!-- Sidebar -->
  <rect x="1" y="42" width="184" height="357" fill="{view}"/>
  <rect x="16" y="64" width="152" height="28" rx="7" fill="{accent}"/>
  <rect x="26" y="74" width="86" height="8" rx="4" fill="{accent_text}"/>
  <rect x="20" y="108" width="112" height="8" rx="4" fill="{text}" opacity="0.75"/>
  <rect x="20" y="132" width="132" height="8" rx="4" fill="{text}" opacity="0.75"/>
  <rect x="20" y="156" width="98" height="8" rx="4" fill="{text}" opacity="0.75"/>
  <rect x="20" y="180" width="120" height="8" rx="4" fill="{text}" opacity="0.75"/>

  <!-- Content: heading and body -->
  <rect x="212" y="72" width="216" height="14" rx="7" fill="{text}"/>
  <rect x="212" y="100" width="360" height="9" rx="4.5" fill="{muted}"/>
  <rect x="212" y="118" width="330" height="9" rx="4.5" fill="{muted}"/>
  <rect x="212" y="136" width="286" height="9" rx="4.5" fill="{muted}"/>

  <!-- Buttons -->
  <rect x="212" y="168" width="118" height="36" rx="8" fill="{accent}"/>
  <rect x="236" y="183" width="70" height="7" rx="3.5" fill="{accent_text}"/>
  <rect x="342" y="168" width="118" height="36" rx="8" fill="{hover}"/>
  <rect x="366" y="183" width="70" height="7" rx="3.5" fill="{text}"/>

  <!-- A warning line -->
  <circle cx="220" cy="236" r="5" fill="{warning}"/>
  <rect x="234" y="232" width="168" height="8" rx="4" fill="{warning}"/>

  <!-- Palette strip: every promised colour -->
  <rect x="212" y="270" width="52" height="20" rx="5" fill="{accent}"/>
  <rect x="272" y="270" width="52" height="20" rx="5" fill="{background}" stroke="{muted}" stroke-width="1"/>
  <rect x="332" y="270" width="52" height="20" rx="5" fill="{surface}" stroke="{muted}" stroke-width="1"/>
  <rect x="392" y="270" width="52" height="20" rx="5" fill="{button}"/>
  <rect x="452" y="270" width="52" height="20" rx="5" fill="{hover}"/>
  <rect x="512" y="270" width="52" height="20" rx="5" fill="{text}"/>
  <rect x="212" y="300" width="352" height="2" rx="1" fill="{muted}" opacity="0.4"/>
  </g>

  <!-- Window outline, drawn last so the clipped content cannot cover it -->
  <rect x="0.5" y="0.5" width="639" height="399" rx="18" fill="none" stroke="{muted}" stroke-opacity="0.35"/>
</svg>
"#
    )
}

/// The `AccentColor` value Plasma 6 expects, as `r,g,b`.
///
/// Plasma 6 does not take a scheme's accent from `[Colors:Selection]`. It reads a
/// separate `AccentColor` key in `[General]`, and `plasma-apply-colorscheme` only
/// writes it when the scheme file carries it. Without this key the shell keeps
/// whatever accent it had — which is why the Kickoff selection pill and the
/// category icons stayed blue while Qt applications picked up the new palette.
pub fn accent_key(palette: &Palette) -> String {
    palette.accent.solid().to_kde_triplet()
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
    use ws_palette::{build, Rgb, TEXT_CONTRAST_TARGET};

    fn report(variant: Variant) -> ws_palette::PaletteReport {
        build(Rgb::from_hex("#5f6fdd").unwrap(), variant, 255)
    }

    fn triplet_contrast(a: &str, b: &str) -> f32 {
        let parse = |s: &str| {
            let mut parts = s.split(',').map(|p| p.trim().parse::<u8>().unwrap());
            Rgb::rgb(
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
            )
        };
        parse(a).contrast_ratio(parse(b))
    }

    #[test]
    fn generated_scheme_has_every_required_section() {
        let scheme = generate(&report(Variant::Dark).palette, Variant::Dark);
        let parsed = ParsedScheme::parse(&scheme.text);
        for section in COLOR_SECTIONS {
            assert!(
                parsed.sections.iter().any(|s| s == section),
                "missing {section}"
            );
        }
        assert!(parsed.sections.iter().any(|s| s == "General"));
        assert!(parsed.sections.iter().any(|s| s == "ColorEffects:Disabled"));
        assert!(parsed.sections.iter().any(|s| s == "ColorEffects:Inactive"));
    }

    #[test]
    fn every_colour_section_carries_the_full_key_set() {
        let scheme = generate(&report(Variant::Dark).palette, Variant::Dark);
        let parsed = ParsedScheme::parse(&scheme.text);
        const REQUIRED: [&str; 12] = [
            "BackgroundNormal",
            "BackgroundAlternate",
            "ForegroundNormal",
            "ForegroundInactive",
            "ForegroundActive",
            "ForegroundLink",
            "ForegroundVisited",
            "ForegroundNegative",
            "ForegroundNeutral",
            "ForegroundPositive",
            "DecorationFocus",
            "DecorationHover",
        ];
        for section in COLOR_SECTIONS {
            for key in REQUIRED {
                assert!(
                    parsed.get(section, key).is_some(),
                    "{section} missing {key}"
                );
            }
        }
    }

    #[test]
    fn colours_are_written_as_rgb_triplets() {
        let scheme = generate(&report(Variant::Dark).palette, Variant::Dark);
        let parsed = ParsedScheme::parse(&scheme.text);
        for section in COLOR_SECTIONS {
            let value = parsed.get(section, "BackgroundNormal").unwrap();
            let parts: Vec<&str> = value.split(',').collect();
            assert_eq!(parts.len(), 3, "{section}: {value}");
            for part in parts {
                part.parse::<u8>()
                    .unwrap_or_else(|_| panic!("{section}: bad component in {value}"));
            }
        }
    }

    #[test]
    fn text_remains_readable_in_every_generated_section() {
        for variant in [Variant::Dark, Variant::Light] {
            let scheme = generate(&report(variant).palette, variant);
            let parsed = ParsedScheme::parse(&scheme.text);
            for section in COLOR_SECTIONS {
                let bg = parsed.get(section, "BackgroundNormal").unwrap();
                let fg = parsed.get(section, "ForegroundNormal").unwrap();
                let ratio = triplet_contrast(fg, bg);
                assert!(
                    ratio >= TEXT_CONTRAST_TARGET,
                    "{variant:?} {section}: {fg} on {bg} = {ratio:.2}"
                );
            }
        }
    }

    #[test]
    fn selection_uses_the_accent_with_its_own_text_colour() {
        let palette = report(Variant::Dark).palette;
        let scheme = generate(&palette, Variant::Dark);
        let parsed = ParsedScheme::parse(&scheme.text);
        assert_eq!(
            parsed.get("Colors:Selection", "BackgroundNormal").unwrap(),
            palette.selection.solid().to_kde_triplet()
        );
        let ratio = triplet_contrast(
            parsed.get("Colors:Selection", "ForegroundNormal").unwrap(),
            parsed.get("Colors:Selection", "BackgroundNormal").unwrap(),
        );
        assert!(
            ratio >= TEXT_CONTRAST_TARGET,
            "selection text ratio {ratio}"
        );
    }

    #[test]
    fn the_scheme_name_changes_with_the_palette_so_plasma_reapplies() {
        let a = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255).palette;
        let b = build(Rgb::from_hex("#dd5fae").unwrap(), Variant::Dark, 255).palette;
        assert_ne!(
            scheme_name(&a, Variant::Dark),
            scheme_name(&b, Variant::Dark)
        );
        // But it is stable for the same palette, so applying twice is idempotent.
        assert_eq!(
            scheme_name(&a, Variant::Dark),
            scheme_name(&a, Variant::Dark)
        );
    }

    #[test]
    fn light_and_dark_names_do_not_collide() {
        let palette = report(Variant::Dark).palette;
        assert_ne!(
            scheme_name(&palette, Variant::Dark),
            scheme_name(&palette, Variant::Light)
        );
    }

    #[test]
    fn the_file_name_matches_the_scheme_name() {
        let scheme = generate(&report(Variant::Dark).palette, Variant::Dark);
        assert_eq!(scheme.file_name(), format!("{}.colors", scheme.name));
        let parsed = ParsedScheme::parse(&scheme.text);
        assert_eq!(parsed.name.as_deref(), Some(scheme.name.as_str()));
    }

    #[test]
    fn generation_is_deterministic() {
        let palette = report(Variant::Dark).palette;
        assert_eq!(
            generate(&palette, Variant::Dark),
            generate(&palette, Variant::Dark)
        );
    }

    #[test]
    fn the_scheme_carries_the_accent_key_plasma_needs() {
        // Regression: without `AccentColor` in `[General]`, Plasma keeps its
        // previous accent for the shell. The Kickoff selection pill and the
        // category icons stayed blue while Qt applications switched correctly,
        // because `plasma-apply-colorscheme` only writes the key when the scheme
        // file contains it.
        let palette = report(Variant::Dark).palette;
        let scheme = generate(&palette, Variant::Dark);
        let parsed = ParsedScheme::parse(&scheme.text);
        let accent = parsed
            .get("General", "AccentColor")
            .expect("schemes must carry AccentColor or the shell ignores the palette");
        assert_eq!(accent, palette.accent.solid().to_kde_triplet());
    }

    #[test]
    fn the_accent_key_matches_the_selection_background() {
        // Plasma draws selection with the accent, so the two must agree or the
        // shell and the applications disagree about what "selected" looks like.
        for variant in [Variant::Dark, Variant::Light] {
            let palette = report(variant).palette;
            let scheme = generate(&palette, variant);
            let parsed = ParsedScheme::parse(&scheme.text);
            assert_eq!(
                parsed.get("General", "AccentColor").unwrap(),
                parsed.get("Colors:Selection", "BackgroundNormal").unwrap(),
                "{variant:?}"
            );
        }
    }

    #[test]
    fn the_preview_svg_is_well_formed_and_uses_only_palette_colours() {
        for variant in [Variant::Dark, Variant::Light] {
            let palette = build(Rgb::from_hex("#5f6fdd").unwrap(), variant, 255).palette;
            let svg = preview_svg(&palette, variant);
            assert!(svg.starts_with("<svg"), "not an SVG");
            assert!(svg.trim_end().ends_with("</svg>"), "unterminated SVG");
            // Every colour used comes from the palette (or is a stroke opacity,
            // not a colour), so the export can never drift from the theme.
            let allowed = [
                palette.background,
                palette.surface,
                palette.button,
                palette.hover,
                palette.accent,
                palette.accent_text,
                palette.text,
                palette.text_muted,
                palette.warning,
            ]
            .iter()
            .map(|c| c.solid().to_hex())
            .collect::<Vec<_>>();
            let mut start = 0;
            while let Some(found) = svg[start..].find("fill=\"") {
                let at = start + found + 6;
                let end = at + svg[at..].find('"').expect("unterminated attribute");
                let value = &svg[at..end];
                // `fill="none"` is a legitimate outline; anything else must be a
                // palette colour so the export cannot drift from the theme.
                assert!(
                    value == "none" || allowed.iter().any(|a| a == value),
                    "{variant:?}: {value} is not a palette colour"
                );
                start = end;
            }
        }
    }

    #[test]
    fn the_preview_svg_contains_no_text_elements() {
        // Text would need a font and would embed user-visible strings; the
        // specimen is shapes only so it renders identically everywhere.
        let palette = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255).palette;
        let svg = preview_svg(&palette, Variant::Dark);
        assert!(!svg.contains("<text"), "{svg}");
        assert!(!svg.contains("<foreignObject"), "{svg}");
    }

    #[test]
    fn translucent_background_does_not_leak_alpha_into_the_scheme() {
        // .colors has no alpha channel; a translucent palette must still produce
        // opaque triples rather than something Plasma would reject.
        let palette = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 180).palette;
        assert_ne!(palette.background.a, 255);
        let scheme = generate(&palette, Variant::Dark);
        let parsed = ParsedScheme::parse(&scheme.text);
        for section in COLOR_SECTIONS {
            let value = parsed.get(section, "BackgroundNormal").unwrap();
            assert!(!value.contains('#'), "{section}: alpha leaked as {value}");
            assert_eq!(value.split(',').count(), 3, "{section}: {value}");
        }
    }
}
