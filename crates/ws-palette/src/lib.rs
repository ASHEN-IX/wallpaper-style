//! Wallpaper colour extraction and contrast-safe palette generation.
//!
//! This crate is deliberately free of any desktop-environment knowledge. It
//! takes an image, produces a [`palette::PaletteReport`] and can be tested
//! without KDE, KWin, Kvantum or a display server.
//!
//! ```
//! use ws_palette::{palette, Variant};
//!
//! # let accent = ws_palette::Rgb::from_hex("#5f6fdd").unwrap();
//! let report = palette::build(accent, Variant::Dark, 255);
//! assert!(report.all_pass());
//! ```

#![deny(missing_debug_implementations)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::float_cmp,
    clippy::manual_midpoint,
    clippy::items_after_statements,
    clippy::new_ret_no_self,
    clippy::needless_pass_by_value,
    clippy::return_self_not_must_use,
    clippy::self_named_constructors,
    // The lints below fire only in test modules in this crate, where the flagged
    // forms are deliberate: exhaustive `other => panic!()` guards so a new enum
    // variant cannot pass silently, and `assert!(x.is_empty())` for readability.
    clippy::assert_is_empty,
    clippy::match_wildcard_for_single_variants,
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
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
)]

pub mod color;
pub mod extract;
pub mod oklab;
pub mod palette;
pub mod sample;

pub use color::{Hsl, Rgb};
pub use extract::{extract_accent, AccentSource, Extraction, HuedCluster};
pub use oklab::{Oklab, Oklch};
pub use palette::{
    build, check, ContrastCheck, Palette, PaletteOverrides, PaletteReport, PaletteRole, Variant,
    TEXT_CONTRAST_TARGET,
};
pub use sample::{sample_bytes, sample_image, sample_path, ImageError, SampledImage};

/// Everything needed to render a theme, produced in one call.
#[derive(Debug, Clone, PartialEq)]
pub struct Derivation {
    pub accent: Rgb,
    pub source: AccentSource,
    pub clusters: Vec<HuedCluster>,
    pub dark: PaletteReport,
    pub light: PaletteReport,
}

/// Derive the accent and both variants from a decoded wallpaper.
///
/// `preferred_hue` is used only when the image itself has no usable hue, which
/// keeps a slideshow from flashing the fallback accent between frames.
pub fn derive(
    sampled: &SampledImage,
    preferred_hue: Option<f32>,
    surface_alpha: u8,
    overrides: &PaletteOverrides,
) -> Derivation {
    let extraction = extract_accent(sampled, preferred_hue);
    // An explicit accent override replaces the extracted one before contrast
    // repair, so a manual colour is still made readable rather than trusted
    // blindly.
    let accent = overrides.accent.unwrap_or(extraction.accent);
    Derivation {
        accent,
        source: extraction.source,
        clusters: extraction.clusters,
        dark: build_variant(accent, Variant::Dark, surface_alpha, overrides),
        light: build_variant(accent, Variant::Light, surface_alpha, overrides),
    }
}

/// Build one variant, apply overrides, then re-check contrast so the report
/// describes the palette that will actually be applied.
fn build_variant(
    accent: Rgb,
    variant: Variant,
    surface_alpha: u8,
    overrides: &PaletteOverrides,
) -> PaletteReport {
    let base = palette::build(accent, variant, surface_alpha).palette;
    let palette = overrides.apply(base);
    PaletteReport {
        palette,
        variant,
        checks: palette::check(&palette, variant),
    }
}
