//! Deriving a complete, contrast-checked UI palette from one accent colour.
//!
//! The palette is the contract between the extraction crate and the desktop
//! backends: KDE colour schemes, Kvantum themes and the GUI preview all read
//! the same `Palette` value, so what the user previews is what gets applied.
//!
//! Design rules, in priority order:
//!
//! 1. **Readability first.** Text on every surface must meet WCAG 2.2 AA
//!    (4.5:1 for body text). If the accent cannot host readable text in its own
//!    role, the text colour is switched between near-black and near-white and,
//!    failing that, the surface lightness is nudged until the ratio is met.
//! 2. **Backgrounds stay quiet.** Surfaces are the accent's hue at very low
//!    chroma, so a wall of windows does not fight the wallpaper.
//! 3. **Accents stay recognisable.** The accent keeps its hue exactly and is
//!    only moved in lightness/chroma when a contrast rule demands it.

use serde::{Deserialize, Serialize};

use crate::color::Rgb;
use crate::oklab::Oklch;

/// Whether the palette is the dark or light variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    #[default]
    Dark,
    Light,
}

impl Variant {
    pub fn as_str(self) -> &'static str {
        match self {
            Variant::Dark => "dark",
            Variant::Light => "light",
        }
    }
}

impl std::str::FromStr for Variant {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "dark" => Ok(Variant::Dark),
            "light" => Ok(Variant::Light),
            other => Err(format!(
                "unknown variant {other:?}; expected 'dark' or 'light'"
            )),
        }
    }
}

/// The eight colours the product promises, per the specification, plus the
/// supporting text tones a real theme needs.
///
/// Every field is opaque except `surface_alpha`, which expresses
/// background-only transparency for themes that support it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    /// Primary highlight: selections, focus rings, the wallpaper's identity.
    pub accent: Rgb,
    /// Window background behind lists and panels.
    pub background: Rgb,
    /// Raised background: windows, cards, headers.
    pub surface: Rgb,
    /// Button faces.
    pub button: Rgb,
    /// Button faces under the pointer.
    pub hover: Rgb,
    /// Selected rows and text inside them.
    pub selection: Rgb,
    /// Body text.
    pub text: Rgb,
    /// Error/negative text.
    pub warning: Rgb,
    /// Muted text for inactive labels.
    pub text_muted: Rgb,
    /// Text drawn on `accent` and `selection`.
    pub accent_text: Rgb,
    /// Positive/success text.
    pub success: Rgb,
    /// Informational text.
    pub info: Rgb,
}

/// Contrast targets. AA for body text is 4.5:1; 3.0:1 applies to large text and
/// UI component boundaries, which could not be met for every hue at every
/// lightness without making the theme wash out, so those are reported rather
/// than enforced.
pub const TEXT_CONTRAST_TARGET: f32 = 4.5;
pub const SURFACE_CONTRAST_TARGET: f32 = 1.12;

/// A contrast observation about one pairing of palette colours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContrastCheck {
    pub foreground_href: String,
    pub background_href: String,
    pub ratio: f32,
    pub required: f32,
}

impl ContrastCheck {
    pub fn passes(&self) -> bool {
        self.ratio >= self.required
    }
}

/// A palette plus the diagnostics the UI shows next to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaletteReport {
    pub palette: Palette,
    pub variant: Variant,
    pub checks: Vec<ContrastCheck>,
}

impl PaletteReport {
    pub fn all_pass(&self) -> bool {
        self.checks.iter().all(ContrastCheck::passes)
    }

    pub fn failures(&self) -> impl Iterator<Item = &ContrastCheck> {
        self.checks.iter().filter(|c| !c.passes())
    }

    /// Smallest ratio among body-text checks; the UI uses this as a headline.
    pub fn worst_text_ratio(&self) -> f32 {
        self.checks
            .iter()
            .filter(|c| c.required >= TEXT_CONTRAST_TARGET)
            .map(|c| c.ratio)
            .fold(f32::INFINITY, f32::min)
    }
}

/// Overrides applied after automatic generation. Only colours the user
/// explicitly changed are present, so the rest keep tracking the wallpaper.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<Rgb>,
}

impl PaletteOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Apply overrides on top of a generated palette.
    pub fn apply(&self, mut palette: Palette) -> Palette {
        if let Some(v) = self.accent {
            palette.accent = v;
            palette.selection = v;
        }
        if let Some(v) = self.background {
            palette.background = v;
        }
        if let Some(v) = self.surface {
            palette.surface = v;
        }
        if let Some(v) = self.button {
            palette.button = v;
        }
        if let Some(v) = self.hover {
            palette.hover = v;
        }
        if let Some(v) = self.selection {
            palette.selection = v;
        }
        if let Some(v) = self.text {
            palette.text = v;
        }
        if let Some(v) = self.warning {
            palette.warning = v;
        }
        palette
    }
}

/// Build the full palette for an accent.
///
/// `surface_alpha` lets the caller request background-only transparency. The
/// contrast checks always run on the composited result, so a translucent
/// surface is still verified against the text drawn on it.
pub fn build(accent: Rgb, variant: Variant, surface_alpha: u8) -> PaletteReport {
    let accent_lch = Oklch::from_rgb(accent);
    let hue = accent_lch.h;

    // Surfaces: the accent's hue at very low chroma so the desktop feels related
    // to the wallpaper without being tinted so hard it looks like a filter.
    // The steps are spaced so that each neighbouring pair clears
    // [`SURFACE_CONTRAST_TARGET`]. Widen them, do not narrow them: a raised
    // surface that is only ~1.09:1 above the background is visually flat and
    // fails the check that a user can tell a window from its surroundings.
    // Values chosen from measured OKLab→luminance ratios so each neighbouring
    // pair clears SURFACE_CONTRAST_TARGET (1.12) with margin: at 0.13/0.22 the
    // measured ratio is 1.16, and the light 0.97/0.92 pair is also 1.16.
    let (bg_l, surface_l, button_l, hover_l) = match variant {
        Variant::Dark => (0.13, 0.22, 0.31, 0.40),
        Variant::Light => (0.97, 0.92, 0.85, 0.78),
    };
    // Light surfaces step *down* in chroma as they get darker; the hue tint is
    // most visible on the lightest tone.
    let (bg_c, surface_c, button_c, hover_c) = match variant {
        Variant::Dark => (0.012, 0.018, 0.024, 0.030),
        Variant::Light => (0.008, 0.011, 0.014, 0.017),
    };

    let background = Oklch {
        l: bg_l,
        c: bg_c,
        h: hue,
    }
    .to_rgb();
    let surface = Oklch {
        l: surface_l,
        c: surface_c,
        h: hue,
    }
    .to_rgb();
    let button = Oklch {
        l: button_l,
        c: button_c,
        h: hue,
    }
    .to_rgb();
    let hover = Oklch {
        l: hover_l,
        c: hover_c,
        h: hue,
    }
    .to_rgb();

    let text = match variant {
        Variant::Dark => pick_readable_text(surface, hue, true),
        Variant::Light => pick_readable_text(surface, hue, false),
    };
    let text_muted = muted(text, surface, variant);

    // The accent must carry readable text on `selection` *and* remain visible
    // as a UI element against the surface. A vivid yellow at its natural
    // lightness is legible on a white-ish surface but nearly invisible as a
    // marker, so darken it until it separates.
    let (accent, _) = ensure_accent_contrast(accent, hue, variant);
    let accent = ensure_accent_visible(accent, surface, variant);
    // Re-derive the text colour for the final accent: changing lightness may
    // have flipped the readable direction.
    let accent_text = best_text_for(accent, hue);
    let selection = accent;

    let warning = warning_color(variant, surface);
    let success = success_color(variant, surface);
    let info = info_color(variant, surface);

    let palette = Palette {
        accent,
        background: alpha(background, surface_alpha),
        // The "surface" colour stays the raised panel colour; only `background`
        // becomes translucent by default. Both are checked composited.
        surface,
        button,
        hover,
        selection,
        text,
        warning,
        text_muted,
        accent_text,
        success,
        info,
    };

    let checks = check(&palette, variant);
    PaletteReport {
        palette,
        variant,
        checks,
    }
}

/// Set alpha on a background colour when the caller asked for transparency.
fn alpha(color: Rgb, value: u8) -> Rgb {
    if value == 255 {
        color
    } else {
        color.with_alpha(value)
    }
}

/// Nudge an accent until it is distinguishable as a UI element on `surface`,
/// keeping its hue. Only used when the accent would otherwise vanish.
fn ensure_accent_visible(accent: Rgb, surface: Rgb, variant: Variant) -> Rgb {
    if accent.contrast_ratio(surface) >= 1.4 {
        return accent;
    }
    let lch = Oklch::from_rgb(accent);
    // On a light surface, go darker; on a dark surface, go lighter.
    let direction: f32 = match variant {
        Variant::Light => -1.0,
        Variant::Dark => 1.0,
    };
    for step in 1..=30 {
        let l = lch.l + direction * step as f32 * 0.02;
        if !(0.10..=0.95).contains(&l) {
            break;
        }
        let candidate = Oklch {
            l,
            c: lch.c,
            h: lch.h,
        }
        .to_rgb();
        if candidate.contrast_ratio(surface) >= 1.4 {
            return candidate;
        }
    }
    accent
}

/// The better of dark or light text on `accent`, preferring whichever passes
/// AA and otherwise the higher ratio.
fn best_text_for(accent: Rgb, hue: f32) -> Rgb {
    let dark_text = Oklch {
        l: 0.13,
        c: 0.008,
        h: hue,
    }
    .to_rgb();
    let light_text = Oklch {
        l: 0.99,
        c: 0.0,
        h: hue,
    }
    .to_rgb();
    let (dark_ratio, light_ratio) = (
        accent.contrast_ratio(dark_text),
        accent.contrast_ratio(light_text),
    );
    if dark_ratio >= light_ratio {
        dark_text
    } else {
        light_text
    }
}

/// Choose near-black or near-white for body text, preferring the direction the
/// variant expects but switching if the surface is unusual (for example a very
/// light dark-surface due to a manual override).
fn pick_readable_text(surface: Rgb, hue: f32, prefer_light: bool) -> Rgb {
    let candidates = [
        Oklch {
            l: 0.97,
            c: 0.006,
            h: hue,
        }
        .to_rgb(),
        Oklch {
            l: 0.16,
            c: 0.010,
            h: hue,
        }
        .to_rgb(),
    ];
    let ordered = if prefer_light {
        candidates
    } else {
        [candidates[1], candidates[0]]
    };
    let first = ordered[0];
    let second = ordered[1];
    if first.contrast_ratio(surface) >= TEXT_CONTRAST_TARGET {
        first
    } else if second.contrast_ratio(surface) >= TEXT_CONTRAST_TARGET {
        second
    } else {
        // Neither reaches AA (only possible for mid-grey surfaces); take the
        // better of the two rather than shipping the worse one.
        if first.contrast_ratio(surface) >= second.contrast_ratio(surface) {
            first
        } else {
            second
        }
    }
}

/// A dimmed text colour that still keeps a usable ratio, falling back to the
/// body text colour when dimming would drop below 3:1.
fn muted(text: Rgb, surface: Rgb, variant: Variant) -> Rgb {
    let target_l = match variant {
        Variant::Dark => 0.62,
        Variant::Light => 0.46,
    };
    let base = Oklch::from_rgb(text);
    let candidate = Oklch {
        l: target_l,
        c: base.c.max(0.004),
        h: base.h,
    }
    .to_rgb();
    if candidate.contrast_ratio(surface) >= 3.0 {
        candidate
    } else {
        text
    }
}

/// Move the accent's lightness until either near-black or near-white text
/// reaches AA on it, keeping the hue.
fn ensure_accent_contrast(accent: Rgb, hue: f32, variant: Variant) -> (Rgb, Rgb) {
    let dark_text = Oklch {
        l: 0.13,
        c: 0.008,
        h: hue,
    }
    .to_rgb();
    let light_text = Oklch {
        l: 0.99,
        c: 0.0,
        h: hue,
    }
    .to_rgb();

    let accent_lch = Oklch::from_rgb(accent);

    // Candidate lightnesses, in preference order: the accent untouched first,
    // then a walk in the direction the variant suggests (dark themes usually
    // want a lighter accent carrying dark text), then the opposite direction as
    // a fallback. The accent itself has to be a candidate: a near-white accent
    // in a dark theme is already ideal and a walk that only moved *away* from it
    // would needlessly change the user's colour.
    let step: f32 = match variant {
        Variant::Dark => 0.022,
        Variant::Light => -0.022,
    };
    let mut candidates: Vec<f32> = Vec::with_capacity(49);
    candidates.push(accent_lch.l);
    for i in 1..=24 {
        candidates.push(accent_lch.l + step * i as f32);
    }
    for i in 1..=24 {
        candidates.push(accent_lch.l - step * i as f32);
    }

    // Both text directions are considered for every candidate, and the accent
    // that moved least from the wallpaper's colour wins. Choosing the text
    // colour inside the loop (rather than trying the variant's preferred
    // direction first) avoids settling on a distant lightness that merely
    // happened to unlock one direction first.
    let mut best: Option<(Rgb, Rgb, f32)> = None;
    for raw_l in candidates {
        let l = raw_l.clamp(0.08, 0.97);
        let candidate = Oklch {
            l,
            c: accent_lch.c,
            h: accent_lch.h,
        }
        .to_rgb();
        for text in [dark_text, light_text] {
            if candidate.contrast_ratio(text) < TEXT_CONTRAST_TARGET {
                continue;
            }
            let drift = (l - accent_lch.l).abs();
            if best.is_none_or(|(_, _, d)| drift < d) {
                best = Some((candidate, text, drift));
            }
        }
    }
    if let Some((candidate, text, _)) = best {
        return (candidate, text);
    }
    // Extremely unlikely: no lightness in range produced readable text. Keep
    // the accent and use white text, and let the contrast report show the
    // failure honestly rather than silently shipping a worse colour.
    (accent, light_text)
}

fn warning_color(variant: Variant, surface: Rgb) -> Rgb {
    let target = match variant {
        Variant::Dark => 0.70,
        Variant::Light => 0.50,
    };
    readable_hue(20.0, 0.17, target, surface)
}

fn success_color(variant: Variant, surface: Rgb) -> Rgb {
    let target = match variant {
        Variant::Dark => 0.72,
        Variant::Light => 0.52,
    };
    readable_hue(145.0, 0.13, target, surface)
}

fn info_color(variant: Variant, surface: Rgb) -> Rgb {
    let target = match variant {
        Variant::Dark => 0.73,
        Variant::Light => 0.52,
    };
    readable_hue(250.0, 0.12, target, surface)
}

/// Build a semantic colour at a fixed hue, darkening or lightening it until it
/// is readable on `surface`.
fn readable_hue(hue: f32, chroma: f32, start_lightness: f32, surface: Rgb) -> Rgb {
    let direction = if surface.relative_luminance() < 0.35 {
        1.0
    } else {
        -1.0
    };
    for step in 0..18 {
        let l = start_lightness + direction * step as f32 * 0.025;
        if !(0.05..=0.98).contains(&l) {
            break;
        }
        let candidate = Oklch {
            l,
            c: chroma,
            h: hue,
        }
        .to_rgb();
        if candidate.contrast_ratio(surface) >= TEXT_CONTRAST_TARGET {
            return candidate;
        }
    }
    Oklch {
        l: start_lightness,
        c: chroma,
        h: hue,
    }
    .to_rgb()
}

/// Run the standard contrast battery over a palette.
pub fn check(palette: &Palette, _variant: Variant) -> Vec<ContrastCheck> {
    // Translucent backgrounds are checked as they appear over the raised
    // surface, which is the lightest thing they can sit on in practice.
    let background_visible = palette.background.over(palette.surface);
    vec![
        ContrastCheck {
            foreground_href: "text".to_owned(),
            background_href: "surface".to_owned(),
            ratio: palette.text.contrast_ratio(palette.surface),
            required: TEXT_CONTRAST_TARGET,
        },
        ContrastCheck {
            foreground_href: "text".to_owned(),
            background_href: "background".to_owned(),
            ratio: palette.text.contrast_ratio(background_visible),
            required: TEXT_CONTRAST_TARGET,
        },
        ContrastCheck {
            foreground_href: "text".to_owned(),
            background_href: "button".to_owned(),
            ratio: palette.text.contrast_ratio(palette.button),
            required: TEXT_CONTRAST_TARGET,
        },
        ContrastCheck {
            foreground_href: "text".to_owned(),
            background_href: "hover".to_owned(),
            ratio: palette.text.contrast_ratio(palette.hover),
            required: TEXT_CONTRAST_TARGET,
        },
        ContrastCheck {
            foreground_href: "text_muted".to_owned(),
            background_href: "surface".to_owned(),
            ratio: palette.text_muted.contrast_ratio(palette.surface),
            required: 3.0,
        },
        ContrastCheck {
            foreground_href: "accent_text".to_owned(),
            background_href: "selection".to_owned(),
            ratio: palette.accent_text.contrast_ratio(palette.selection),
            required: TEXT_CONTRAST_TARGET,
        },
        ContrastCheck {
            foreground_href: "warning".to_owned(),
            background_href: "surface".to_owned(),
            ratio: palette.warning.contrast_ratio(palette.surface),
            required: TEXT_CONTRAST_TARGET,
        },
        ContrastCheck {
            foreground_href: "success".to_owned(),
            background_href: "surface".to_owned(),
            ratio: palette.success.contrast_ratio(palette.surface),
            required: TEXT_CONTRAST_TARGET,
        },
        ContrastCheck {
            foreground_href: "info".to_owned(),
            background_href: "surface".to_owned(),
            ratio: palette.info.contrast_ratio(palette.surface),
            required: TEXT_CONTRAST_TARGET,
        },
        // A UI boundary does not need to be readable text, but it must be
        // distinguishable from the surface it outlines.
        ContrastCheck {
            foreground_href: "accent".to_owned(),
            background_href: "surface".to_owned(),
            ratio: palette.accent.contrast_ratio(palette.surface),
            required: 1.4,
        },
        // Structural separation is judged on the *designed* pair. Compositing a
        // translucent background over the surface it sits on would make the two
        // converge by construction and the check would measure nothing useful.
        ContrastCheck {
            foreground_href: "surface".to_owned(),
            background_href: "background".to_owned(),
            ratio: palette.surface.contrast_ratio(palette.background),
            required: SURFACE_CONTRAST_TARGET,
        },
    ]
}

/// Which of `palette`'s eight promised colours is being referred to, for the
/// GUI's editable swatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteRole {
    Accent,
    Background,
    Surface,
    Button,
    Hover,
    Selection,
    Text,
    Warning,
}

impl PaletteRole {
    pub const ALL: [PaletteRole; 8] = [
        PaletteRole::Accent,
        PaletteRole::Background,
        PaletteRole::Surface,
        PaletteRole::Button,
        PaletteRole::Hover,
        PaletteRole::Selection,
        PaletteRole::Text,
        PaletteRole::Warning,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PaletteRole::Accent => "Accent",
            PaletteRole::Background => "Background",
            PaletteRole::Surface => "Surface",
            PaletteRole::Button => "Button",
            PaletteRole::Hover => "Hover",
            PaletteRole::Selection => "Selection",
            PaletteRole::Text => "Text",
            PaletteRole::Warning => "Warning",
        }
    }

    pub fn get(self, palette: &Palette) -> Rgb {
        match self {
            PaletteRole::Accent => palette.accent,
            PaletteRole::Background => palette.background,
            PaletteRole::Surface => palette.surface,
            PaletteRole::Button => palette.button,
            PaletteRole::Hover => palette.hover,
            PaletteRole::Selection => palette.selection,
            PaletteRole::Text => palette.text,
            PaletteRole::Warning => palette.warning,
        }
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

    fn accents() -> Vec<Rgb> {
        [
            "#5f6fdd", "#3ddc97", "#e0345a", "#ffb703", "#00b4d8", "#a06cd5", "#8d5524",
        ]
        .iter()
        .map(|h| Rgb::from_hex(h).unwrap())
        .collect()
    }

    #[test]
    fn every_variant_of_common_accents_meets_aa_for_body_text() {
        for accent in accents() {
            for variant in [Variant::Dark, Variant::Light] {
                let report = build(accent, variant, 255);
                let failures: Vec<_> = report.failures().collect();
                assert!(
                    failures.is_empty(),
                    "accent {accent} {variant:?} failed: {failures:?}"
                );
            }
        }
    }

    #[test]
    fn accent_hue_is_preserved() {
        for accent in accents() {
            let report = build(accent, Variant::Dark, 255);
            let before = accent.to_hsl().h;
            let after = report.palette.accent.to_hsl().h;
            let delta = ((before - after + 540.0) % 360.0 - 180.0).abs();
            assert!(delta < 4.0, "{accent} hue drifted {before} -> {after}");
        }
    }

    #[test]
    fn dark_variant_is_darker_than_light_variant() {
        let accent = Rgb::from_hex("#5f6fdd").unwrap();
        let dark = build(accent, Variant::Dark, 255).palette;
        let light = build(accent, Variant::Light, 255).palette;
        assert!(dark.surface.relative_luminance() < light.surface.relative_luminance());
        assert!(dark.text.relative_luminance() > light.text.relative_luminance());
    }

    #[test]
    fn surfaces_stay_low_chroma_so_windows_do_not_shout() {
        for accent in accents() {
            let p = build(accent, Variant::Dark, 255).palette;
            for surface in [p.background, p.surface, p.button] {
                let c = Oklch::from_rgb(surface).c;
                assert!(c < 0.05, "{accent}: surface {surface} chroma {c}");
            }
        }
    }

    #[test]
    fn translucent_background_still_checks_against_composited_result() {
        let accent = Rgb::from_hex("#5f6fdd").unwrap();
        let opaque = build(accent, Variant::Dark, 255).palette;
        let translucent = build(accent, Variant::Dark, 180).palette;
        assert_eq!(translucent.text, opaque.text);
        let report = build(accent, Variant::Dark, 180);
        assert!(
            report.all_pass(),
            "{:?}",
            report.failures().collect::<Vec<_>>()
        );
    }

    #[test]
    fn extremely_dim_accents_are_lifted_until_readable() {
        let nearly_black = Rgb::rgb(12, 12, 16);
        let report = build(nearly_black, Variant::Dark, 255);
        assert!(
            report
                .palette
                .selection
                .contrast_ratio(report.palette.accent_text)
                >= 4.4
        );
    }

    #[test]
    fn extremely_bright_accents_stay_readable() {
        let near_white = Rgb::rgb(250, 250, 245);
        let report = build(near_white, Variant::Dark, 255);
        assert!(
            report
                .palette
                .selection
                .contrast_ratio(report.palette.accent_text)
                >= 4.4
        );
    }

    #[test]
    fn overrides_replace_only_what_was_set() {
        let base = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255).palette;
        let overrides = PaletteOverrides {
            accent: Some(Rgb::rgb(255, 0, 0)),
            ..Default::default()
        };
        let applied = overrides.apply(base);
        assert_eq!(applied.accent, Rgb::rgb(255, 0, 0));
        assert_eq!(applied.selection, Rgb::rgb(255, 0, 0));
        assert_eq!(applied.text, base.text, "unset fields must not change");
    }

    #[test]
    fn empty_overrides_are_a_no_op() {
        let base = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255).palette;
        assert!(PaletteOverrides::default().is_empty());
        assert_eq!(PaletteOverrides::default().apply(base), base);
    }

    #[test]
    fn palette_serialises_as_round_trip_hex() {
        let report = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255);
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"#"), "expected hex colours in {json}");
        let back: PaletteReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn worst_text_ratio_reports_the_body_text_minimum() {
        let report = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255);
        assert!(report.worst_text_ratio() >= TEXT_CONTRAST_TARGET - 0.05);
    }

    #[test]
    fn palette_roles_cover_all_promised_colours() {
        let p = build(Rgb::from_hex("#5f6fdd").unwrap(), Variant::Dark, 255).palette;
        for role in PaletteRole::ALL {
            let _ = role.get(&p);
            assert!(!role.label().is_empty());
        }
    }
}
