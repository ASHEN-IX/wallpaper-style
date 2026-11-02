//! Colour representation and perceptual contrast maths.
//!
//! Everything is stored as 8-bit sRGB with an explicit alpha so that generated
//! KDE colour schemes, Kvantum themes and preview widgets all consume the same
//! values. Conversions to linear light happen only inside the contrast and
//! luminance helpers, which keeps the public types cheap to copy and serialise.

use serde::{Deserialize, Serialize};

/// An 8-bit sRGB colour with straight (non-premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgb {
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Build from a hex string in `#rgb`, `#rrggbb` or `#rrggbbaa` form.
    pub fn from_hex(value: &str) -> Option<Self> {
        let s = value.trim().trim_start_matches('#');
        let parse = |slice: &str| u8::from_str_radix(slice, 16).ok();
        match s.len() {
            3 => Some(Self::rgb(
                parse(&s[0..1])? * 17,
                parse(&s[1..2])? * 17,
                parse(&s[2..3])? * 17,
            )),
            6 => Some(Self::rgb(
                parse(&s[0..2])?,
                parse(&s[2..4])?,
                parse(&s[4..6])?,
            )),
            8 => Some(Self::rgba(
                parse(&s[0..2])?,
                parse(&s[2..4])?,
                parse(&s[4..6])?,
                parse(&s[6..8])?,
            )),
            _ => None,
        }
    }

    /// `#rrggbb`, ignoring alpha. Used for KDE colour scheme files, which have
    /// no way to express alpha.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// `#rrggbbaa` when alpha is not opaque, otherwise `#rrggbb`.
    pub fn to_hex_with_alpha(self) -> String {
        if self.a == 255 {
            self.to_hex()
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    /// The `r,g,b` triple that KDE colour scheme files use for every value.
    pub fn to_kde_triplet(self) -> String {
        format!("{},{},{}", self.r, self.g, self.b)
    }

    #[must_use]
    pub fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }

    /// Drop alpha, returning an opaque colour with the same channels.
    ///
    /// Used wherever a target format cannot express transparency, notably KDE
    /// colour scheme files. Compositing is deliberately *not* done here: callers
    /// that need the visible result should use [`Rgb::over`] with the surface
    /// the colour will sit on.
    #[must_use]
    pub fn solid(self) -> Self {
        Self { a: 255, ..self }
    }

    /// Blend `self` over `background` at the given alpha, returning an opaque
    /// colour. Needed whenever a translucent surface must be checked for
    /// contrast against the text drawn on it.
    #[must_use]
    pub fn over(self, background: Rgb) -> Rgb {
        let a = self.a as f32 / 255.0;
        let mix = |fg: u8, bg: u8| {
            (fg as f32 * a + bg as f32 * (1.0 - a))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Rgb::rgb(
            mix(self.r, background.r),
            mix(self.g, background.g),
            mix(self.b, background.b),
        )
    }

    /// Linear-light relative luminance per WCAG 2.2.
    #[must_use]
    pub fn relative_luminance(self) -> f32 {
        let lin = |c: u8| {
            let s = c as f32 / 255.0;
            if s <= 0.040_45 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// WCAG 2.2 contrast ratio between two opaque colours, in `1.0..=21.0`.
    ///
    /// Translucent colours are composited over their companion surface first;
    /// callers that care should use [`Rgb::over`] explicitly.
    #[must_use]
    pub fn contrast_ratio(self, other: Rgb) -> f32 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// HSL hue in degrees, saturation and lightness in `0.0..=1.0`.
    #[must_use]
    pub fn to_hsl(self) -> Hsl {
        let r = self.r as f32 / 255.0;
        let g = self.g as f32 / 255.0;
        let b = self.b as f32 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        if (max - min).abs() < f32::EPSILON {
            return Hsl { h: 0.0, s: 0.0, l };
        }
        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        Hsl { h: h * 60.0, s, l }
    }

    /// True when the colour is effectively grey (no usable hue signal).
    ///
    /// Saturation is measured perceptually rather than with the HSL formula:
    /// near-black colours divide by a tiny max-channel value in HSL and report
    /// a large saturation for a one-step channel difference, so `#080809` would
    /// otherwise look "colourful".
    #[must_use]
    pub fn is_neutral(self) -> bool {
        self.to_oklch().c < 0.012
    }

    /// Perceptual coordinates of this colour.
    #[must_use]
    pub fn to_oklch(self) -> crate::oklab::Oklch {
        crate::oklab::Oklch::from_rgb(self)
    }

    /// Largest per-channel difference, used to measure how far a colour moved.
    #[must_use]
    pub fn channel_distance(self, other: Rgb) -> u8 {
        self.r
            .abs_diff(other.r)
            .max(self.g.abs_diff(other.g))
            .max(self.b.abs_diff(other.b))
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex_with_alpha())
    }
}

impl From<String> for Rgb {
    fn from(value: String) -> Self {
        Rgb::from_hex(&value).unwrap_or_else(|| {
            log::warn!("invalid colour {value:?} in configuration; using mid grey");
            Rgb::rgb(128, 128, 128)
        })
    }
}

impl From<Rgb> for String {
    fn from(value: Rgb) -> Self {
        value.to_hex_with_alpha()
    }
}

/// Hue in degrees `0.0..360.0`, saturation and lightness in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Hsl {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

impl Hsl {
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        let h = self.h.rem_euclid(360.0) / 360.0;
        let s = self.s.clamp(0.0, 1.0);
        let l = self.l.clamp(0.0, 1.0);
        if s.abs() < f32::EPSILON {
            let v = (l * 255.0).round() as u8;
            return Rgb::rgb(v, v, v);
        }
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        let channel = |mut t: f32| {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            let v = if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            };
            (v * 255.0).round().clamp(0.0, 255.0) as u8
        };
        Rgb::rgb(channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
    }

    /// Adjust lightness and saturation while keeping the hue.
    #[must_use]
    pub fn with(self, s: f32, l: f32) -> Self {
        Hsl {
            h: self.h,
            s: s.clamp(0.0, 1.0),
            l: l.clamp(0.0, 1.0),
        }
    }

    /// Signed shortest angular distance from `self` to `other`, in degrees
    /// within `-180.0..=180.0`. Used to cluster hues that wrap past 360°.
    #[must_use]
    pub fn hue_distance(self, other: f32) -> f32 {
        let mut d = (other - self.h) % 360.0;
        if d > 180.0 {
            d -= 360.0;
        } else if d < -180.0 {
            d += 360.0;
        }
        d
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
    fn hex_round_trips() {
        let c = Rgb::rgb(0x5f, 0x6f, 0xdd);
        assert_eq!(c.to_hex(), "#5f6fdd");
        assert_eq!(Rgb::from_hex("#5f6fdd"), Some(c));
        assert_eq!(Rgb::from_hex("5F6FDD"), Some(c));
        assert_eq!(Rgb::from_hex("#fff"), Some(Rgb::rgb(255, 255, 255)));
        assert_eq!(
            Rgb::from_hex("#5f6fdd80"),
            Some(Rgb::rgba(0x5f, 0x6f, 0xdd, 0x80))
        );
    }

    #[test]
    fn invalid_hex_is_rejected_rather_than_guessed() {
        assert_eq!(Rgb::from_hex("#zzzzzz"), None);
        assert_eq!(Rgb::from_hex("#12345"), None);
        assert_eq!(Rgb::from_hex(""), None);
    }

    #[test]
    fn contrast_ratio_matches_wcag_reference_points() {
        let white = Rgb::rgb(255, 255, 255);
        let black = Rgb::rgb(0, 0, 0);
        assert!((white.contrast_ratio(black) - 21.0).abs() < 0.01);
        assert!((white.contrast_ratio(white) - 1.0).abs() < 0.001);
        // #767676 on white is the classic 4.54:1 AA boundary colour.
        let grey = Rgb::rgb(0x76, 0x76, 0x76);
        assert!((grey.contrast_ratio(white) - 4.54).abs() < 0.02);
    }

    #[test]
    fn hsl_round_trips_accurately() {
        for hex in [
            "#5f6fdd", "#3ddc97", "#000000", "#ffffff", "#ff0000", "#123456",
        ] {
            let rgb = Rgb::from_hex(hex).unwrap();
            let back = rgb.to_hsl().to_rgb();
            assert!(
                rgb.channel_distance(back) <= 1,
                "{hex} round-tripped to {back} via {:?}",
                rgb.to_hsl()
            );
        }
    }

    #[test]
    fn hue_distance_wraps_across_zero() {
        let near_red = Hsl {
            h: 350.0,
            s: 0.5,
            l: 0.5,
        };
        assert!((near_red.hue_distance(10.0) - 20.0).abs() < 0.001);
        assert!((near_red.hue_distance(340.0) - (-10.0)).abs() < 0.001);
    }

    #[test]
    fn composite_over_background_matches_expected_mix() {
        let half_white = Rgb::rgba(255, 255, 255, 128);
        let over_black = half_white.over(Rgb::rgb(0, 0, 0));
        assert_eq!(over_black, Rgb::rgb(128, 128, 128));
        let opaque = Rgb::rgb(10, 20, 30);
        assert_eq!(opaque.with_alpha(255).over(Rgb::rgb(255, 255, 255)), opaque);
    }

    #[test]
    fn neutral_detection_ignores_greys() {
        assert!(Rgb::rgb(128, 128, 128).is_neutral());
        assert!(Rgb::rgb(8, 8, 9).is_neutral());
        assert!(!Rgb::rgb(0x5f, 0x6f, 0xdd).is_neutral());
    }
}
