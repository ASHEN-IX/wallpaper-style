//! Accent selection from sampled wallpaper pixels.
//!
//! The prototype took a 24-bucket hue histogram and painted a fixed
//! `HLS(0.62, 0.65)` accent on the winning hue. Two problems surfaced in use:
//!
//! * A wallpaper whose dominant hue is washed out produced a garish accent
//!   unrelated to any pixel actually present.
//! * The accent was never checked against the surfaces it sits on, so text on
//!   buttons could be unreadable.
//!
//! Here the histogram is *weighted* by area and saturation, candidates are
//! ranked by how much of the image they explain, and the winning hue is taken
//! from the densest cluster's own chroma rather than an arbitrary constant. The
//! result is then normalised to a luminance band that works as a UI accent.

use crate::color::{Hsl, Rgb};
use crate::oklab::Oklch;
use crate::sample::{Sample, SampledImage};

/// Fallback accent used when a wallpaper carries no usable hue at all.
///
/// This is deliberately a neutral teal: it reads as "a choice was made" instead
/// of looking like a broken theme, and it is distinguishable from the blues and
/// greys of a typical desktop.
pub const FALLBACK_ACCENT: Rgb = Rgb::rgb(0x3d, 0xdc, 0x97);

/// How the accent was chosen, so the UI can explain unusual results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentSource {
    /// A cluster covering a meaningful part of the image won.
    DominantHue,
    /// Nothing dominated, so the most extreme saturated cluster was used.
    SecondaryHue,
    /// The image is grey or flat; a hue was taken from a near-grey wash that is
    /// too weak to vote for an accent, or a built-in fallback was used.
    Fallback,
}

/// A candidate hue cluster and how much of the image it represents.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HuedCluster {
    pub hue: f32,
    pub chroma: f32,
    pub lightness: f32,
    /// Share of total sample weight in `0.0..=1.0`.
    pub weight: f32,
}

/// Result of accent selection.
#[derive(Debug, Clone, PartialEq)]
pub struct Extraction {
    pub accent: Rgb,
    pub source: AccentSource,
    pub clusters: Vec<HuedCluster>,
}

/// Minimum saturation for a pixel to vote on the hue. Below this the hue is
/// numerically unstable and would scatter across the circle.
const MIN_VOTE_CHROMA: f32 = 0.015;

/// Share of the image's *coloured* pixels the leading hue must explain before
/// it is reported as [`AccentSource::DominantHue`] rather than a secondary hue.
/// Weights are normalised across hue clusters, so this is a plain majority.
const DOMINANT_SHARE: f32 = 0.55;

/// Extract the accent for a sampled wallpaper.
///
/// `preferred_hue` lets a caller pin the hue (for example when the user picked a
/// colour manually or when a slideshow's current frame is unavailable). When it
/// is `Some` and the image has no usable hue, that hue is used instead of the
/// built-in fallback.
pub fn extract_accent(sampled: &SampledImage, preferred_hue: Option<f32>) -> Extraction {
    let clusters = cluster_hues(&sampled.samples);

    if let Some(best) = clusters.first() {
        // Take the hue from the cluster but lift the chroma to a UI-accent level:
        // the densest patch of a photo is usually more muted than a good accent.
        let target_chroma = best.chroma.clamp(0.075, 0.16);
        let accent = Oklch {
            l: accent_lightness(best.lightness),
            c: target_chroma,
            h: best.hue,
        }
        .to_rgb();
        let source = if best.weight >= DOMINANT_SHARE {
            AccentSource::DominantHue
        } else {
            AccentSource::SecondaryHue
        };
        return Extraction {
            accent,
            source,
            clusters,
        };
    }

    // No cluster cleared the voting threshold. A near-grey wash still carries a
    // hue (the "muted mint wallpaper" case), so use it when present; otherwise
    // honour an explicitly preferred hue, and only then fall back to the
    // built-in accent.
    let wash_hue = mean_wash_hue(&sampled.samples);
    let accent = match wash_hue.or(preferred_hue) {
        Some(hue) => Oklch {
            l: 0.68,
            c: 0.115,
            h: hue.rem_euclid(360.0),
        }
        .to_rgb(),
        None => FALLBACK_ACCENT,
    };
    Extraction {
        accent,
        source: AccentSource::Fallback,
        clusters,
    }
}

/// Mean hue (and dominant lightness) of pixels that carry *some* chroma but not
/// enough to vote, weighted by that chroma. Returns `None` for a genuinely grey
/// image.
fn mean_wash_hue(samples: &[Sample]) -> Option<f32> {
    // Circular mean, not a linear average: 350° and 10° must average to 0°.
    let mut x = 0.0_f32;
    let mut y = 0.0_f32;
    let mut weight = 0.0_f32;
    for sample in samples {
        let c = sample.lch.c;
        if !(0.004..MIN_VOTE_CHROMA).contains(&c) {
            continue;
        }
        let rad = sample.lch.h.to_radians();
        let w = c * sample.weight;
        x += w * rad.cos();
        y += w * rad.sin();
        weight += w;
    }
    if weight <= 1e-6 || (x.abs() < 1e-9 && y.abs() < 1e-9) {
        return None;
    }
    Some(y.atan2(x).to_degrees().rem_euclid(360.0))
}

/// Luminance an accent should sit at. Accents that are too dark vanish on dark
/// surfaces; too light and they lose saturation against white text.
fn accent_lightness(image_lightness: f32) -> f32 {
    // Keep accents in a band that works on both variants; bias slightly towards
    // the wallpaper's own brightness so very dark wallpapers do not produce a
    // glaring accent.
    let bias = (image_lightness - 0.4).clamp(-0.1, 0.1) * 0.35;
    (0.66 + bias).clamp(0.55, 0.76)
}

/// Weighted 1-D clustering of hues around the colour circle.
///
/// A fixed 24-bucket histogram (the prototype's approach) splits clusters that
/// straddle a bucket boundary and makes a two-hue image look like a tie. Here
/// buckets are seeded from a finer histogram and then merged when their centres
/// are within `MERGE_DEGREES`, so a cluster spanning 350°..10° stays intact.
pub fn cluster_hues(samples: &[Sample]) -> Vec<HuedCluster> {
    const SEED_BUCKETS: usize = 72; // 5° seeds
    const MERGE_DEGREES: f32 = 22.0;

    // `seed_weight` deliberately accumulates *area* weight only. Scaling it by
    // chroma here made a cluster's reported `weight` mean "area × saturation",
    // which then read as a share of the image and could never reach
    // [`DOMINANT_SHARE`] for a legitimate majority hue. Chroma is tracked
    // separately and used only as a ranking tie-break and for the cluster's own
    // reported colour.
    let mut seed_weight = [0.0_f32; SEED_BUCKETS];
    let mut seed_chroma = [0.0_f32; SEED_BUCKETS];
    let mut seed_lightness = [0.0_f32; SEED_BUCKETS];
    let mut total_weight = 0.0_f32;

    for sample in samples {
        total_weight += sample.weight;
        if sample.lch.c < MIN_VOTE_CHROMA {
            continue;
        }
        let idx =
            ((sample.lch.h / (360.0 / SEED_BUCKETS as f32)).floor() as usize).min(SEED_BUCKETS - 1);
        let w = sample.weight;
        seed_weight[idx] += w;
        seed_chroma[idx] += w * sample.lch.c;
        // Use the perceptual lightness; HSL lightness would over-weight yellows.
        seed_lightness[idx] += w * sample.lch.l;
    }

    if total_weight <= 0.0 {
        return Vec::new();
    }

    let mut clusters: Vec<HuedCluster> = (0..SEED_BUCKETS)
        .filter(|&i| seed_weight[i] > 0.0)
        .map(|i| {
            let bin_degrees = 360.0 / SEED_BUCKETS as f32;
            HuedCluster {
                hue: (i as f32 + 0.5) * bin_degrees,
                chroma: seed_chroma[i] / seed_weight[i],
                lightness: seed_lightness[i] / seed_weight[i],
                weight: seed_weight[i],
            }
        })
        .collect();

    merge_adjacent(&mut clusters, MERGE_DEGREES);

    // Report each cluster's weight as its share of the *hued* pixels, which is
    // the population it was drawn from. Shares therefore sum to 1.0 and
    // [`DOMINANT_SHARE`] means "this hue explains most of the colour in the
    // image", independent of how much grey area surrounds it.
    let hued_weight: f32 = clusters.iter().map(|c| c.weight).sum();
    if hued_weight <= 0.0 {
        return Vec::new();
    }
    for cluster in &mut clusters {
        cluster.weight /= hued_weight;
    }
    clusters.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Tie-break on chroma so a saturated stripe beats a duller equal-area
            // region, then on hue for determinism across runs.
            .then_with(|| {
                b.chroma
                    .partial_cmp(&a.chroma)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a.hue
                    .partial_cmp(&b.hue)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    clusters
}

/// Merge clusters whose centres are within `max_degrees` on the circle,
/// repeatedly, until stable. Weights combine with a chroma-weighted mean hue.
fn merge_adjacent(clusters: &mut Vec<HuedCluster>, max_degrees: f32) {
    loop {
        // Find the *closest* adjacent pair, not merely the first one. Merging
        // in index order could chain a 15° step into a 40° cluster and drag the
        // hue across two unrelated colours; always taking the closest pair
        // keeps merges local.
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let h1 = Hsl {
                    h: clusters[i].hue,
                    s: 1.0,
                    l: 0.5,
                };
                let distance = h1.hue_distance(clusters[j].hue).abs();
                if distance <= max_degrees && best.is_none_or(|(_, _, d)| distance < d) {
                    best = Some((i, j, distance));
                }
            }
        }
        let Some((i, j, _)) = best else { break };

        let a = clusters[i];
        let b = clusters[j];
        let wa = a.weight * a.chroma.max(1e-4);
        let wb = b.weight * b.chroma.max(1e-4);
        let total_w = a.weight + b.weight;
        let total_wc = wa + wb;
        // Interpolate the hue along the shortest arc so 350° and 10° average to
        // 0°, not 180°.
        let h1 = Hsl {
            h: a.hue,
            s: 1.0,
            l: 0.5,
        };
        let delta = h1.hue_distance(b.hue);
        let weight_share = if total_wc > 0.0 { wb / total_wc } else { 0.5 };
        clusters[i] = HuedCluster {
            hue: (a.hue + delta * weight_share).rem_euclid(360.0),
            chroma: (a.chroma * wa + b.chroma * wb) / total_wc.max(1e-6),
            lightness: (a.lightness * a.weight + b.lightness * b.weight) / total_w.max(1e-6),
            weight: total_w,
        };
        clusters.remove(j);
    }
}

/// Convenience: extract a palette-ready accent from raw samples.
pub fn accent_from_samples(samples: &[Sample], preferred_hue: Option<f32>) -> Rgb {
    let sampled = SampledImage {
        width: 0,
        height: 0,
        samples: samples.to_vec(),
        opaque_fraction: 1.0,
        is_monochrome: false,
    };
    extract_accent(&sampled, preferred_hue).accent
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
    use crate::sample::{sample_image, ImageOrientation as Orientation};
    use image::{DynamicImage, RgbImage, RgbaImage};

    fn image_of(colors: &[(Rgb, u32)]) -> DynamicImage {
        let mut img = RgbImage::new(120, 60);
        let mut x = 0u32;
        for (color, width) in colors {
            for _ in 0..*width {
                for y in 0..60 {
                    if x < 120 {
                        img.put_pixel(x, y, image::Rgb([color.r, color.g, color.b]));
                    }
                }
                x += 1;
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    fn accent_for(colors: &[(Rgb, u32)]) -> Extraction {
        let sampled = sample_image(&image_of(colors), Orientation::NoTransforms);
        extract_accent(&sampled, None)
    }

    #[test]
    fn dominant_hue_wins_over_minority() {
        // Three quarters blue, one quarter red: the accent must be blue and the
        // source must say the hue dominated rather than merely led.
        let blue = Rgb::rgb(20, 40, 220);
        let red = Rgb::rgb(220, 30, 30);
        let result = accent_for(&[(blue, 90), (red, 30)]);
        let hsl = result.accent.to_hsl();
        assert!(
            hsl.h > 200.0 && hsl.h < 280.0,
            "expected a blue accent, got {} (hue {})",
            result.accent,
            hsl.h
        );
        assert_eq!(result.source, AccentSource::DominantHue);
        assert!(
            result.clusters[0].weight > 0.6,
            "blue share was {}",
            result.clusters[0].weight
        );
    }

    #[test]
    fn an_even_split_is_reported_as_a_secondary_hue() {
        // With no clear majority the accent still comes from the strongest
        // cluster, but the source records that it was not dominant so the UI can
        // explain a surprising colour.
        let result = accent_for(&[(Rgb::rgb(20, 40, 220), 60), (Rgb::rgb(220, 30, 30), 60)]);
        assert_eq!(result.source, AccentSource::SecondaryHue);
    }

    #[test]
    fn accent_tracks_the_wallpaper_hue_not_a_constant() {
        let green = accent_for(&[(Rgb::rgb(20, 200, 80), 120)]).accent.to_hsl();
        let purple = accent_for(&[(Rgb::rgb(150, 40, 220), 120)]).accent.to_hsl();
        assert!((green.h - 140.0).abs() < 25.0, "green hue {}", green.h);
        assert!((purple.h - 285.0).abs() < 25.0, "purple hue {}", purple.h);
    }

    #[test]
    fn accent_lands_in_a_usable_luminance_band() {
        for color in [
            Rgb::rgb(255, 0, 0),
            Rgb::rgb(0, 0, 255),
            Rgb::rgb(255, 255, 0),
            Rgb::rgb(0, 255, 255),
            Rgb::rgb(120, 0, 160),
        ] {
            let accent = accent_for(&[(color, 120)]).accent;
            let l = accent.relative_luminance();
            assert!(
                (0.15..0.75).contains(&l),
                "accent {accent} for {color} had luminance {l}"
            );
        }
    }

    #[test]
    fn monochrome_wallpaper_falls_back_cleanly() {
        let sampled = sample_image(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(64, 64, image::Rgb([128, 128, 128]))),
            Orientation::NoTransforms,
        );
        let result = extract_accent(&sampled, None);
        assert_eq!(result.source, AccentSource::Fallback);
        assert_eq!(result.accent, FALLBACK_ACCENT);
        assert!(result.clusters.is_empty());
    }

    #[test]
    fn monochrome_wallpaper_uses_preferred_hue_when_given() {
        let sampled = sample_image(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(64, 64, image::Rgb([40, 40, 40]))),
            Orientation::NoTransforms,
        );
        let result = extract_accent(&sampled, Some(200.0));
        assert_eq!(result.source, AccentSource::Fallback);
        assert!(
            (result.accent.to_oklch().h - 200.0).abs() < 8.0,
            "got {}",
            result.accent
        );
    }

    #[test]
    fn fully_transparent_wallpaper_falls_back() {
        let sampled = sample_image(
            &DynamicImage::ImageRgba8(RgbaImage::from_pixel(32, 32, image::Rgba([0, 0, 0, 0]))),
            Orientation::NoTransforms,
        );
        let result = extract_accent(&sampled, None);
        assert_eq!(result.source, AccentSource::Fallback);
        assert_eq!(result.accent, FALLBACK_ACCENT);
    }

    #[test]
    fn saturated_stripe_beats_equal_area_grey() {
        let sampled = sample_image(
            &image_of(&[(Rgb::rgb(128, 128, 128), 60), (Rgb::rgb(230, 120, 20), 60)]),
            Orientation::NoTransforms,
        );
        let result = extract_accent(&sampled, None);
        let hue = result.accent.to_hsl().h;
        assert!(
            (20.0..55.0).contains(&hue),
            "expected orange, got hue {hue}"
        );
    }

    #[test]
    fn neighbouring_hues_form_one_cluster() {
        // Clustering runs in OKLCH hue, where pure red sits near 29° rather than
        // 0°. Two reds 4° apart there must merge into a single cluster.
        let a = Rgb::from_hex("#e04040").unwrap(); // OKLCH hue ~25.3 deg
        let b = Rgb::from_hex("#e02030").unwrap(); // OKLCH hue ~24.5 deg
        let result = accent_for(&[(a, 60), (b, 60)]);
        assert_eq!(result.clusters.len(), 1, "clusters: {:?}", result.clusters);
    }

    #[test]
    fn distant_hues_stay_separate() {
        // Red (~29 deg) and blue (~264 deg) are far apart and must not merge.
        let result = accent_for(&[(Rgb::rgb(230, 60, 60), 60), (Rgb::rgb(40, 70, 230), 60)]);
        assert_eq!(result.clusters.len(), 2, "clusters: {:?}", result.clusters);
    }

    #[test]
    fn extraction_is_deterministic() {
        let colors = [(Rgb::rgb(20, 40, 220), 60), (Rgb::rgb(220, 30, 30), 60)];
        let first = accent_for(&colors);
        let second = accent_for(&colors);
        assert_eq!(first, second);
    }

    #[test]
    fn two_strong_hues_report_both_clusters() {
        let result = accent_for(&[(Rgb::rgb(20, 40, 220), 60), (Rgb::rgb(220, 30, 30), 60)]);
        assert!(result.clusters.len() >= 2, "got {:?}", result.clusters);
        // Weights are shares of the image's coloured pixels, so an even split
        // must land near 50/50 rather than favouring either hue.
        let top = result.clusters[0].weight;
        assert!((0.4..=0.6).contains(&top), "top share was {top}");
    }

    #[test]
    fn cluster_weights_sum_to_one_across_coloured_pixels() {
        // Weights are shares of the coloured population, which is what
        // DOMINANT_SHARE thresholds are expressed against.
        let result = accent_for(&[(Rgb::rgb(20, 40, 220), 90), (Rgb::rgb(220, 30, 30), 30)]);
        let sum: f32 = result.clusters.iter().map(|c| c.weight).sum();
        assert!((sum - 1.0).abs() < 0.01, "weights summed to {sum}");
    }

    #[test]
    fn grey_area_does_not_dilute_a_dominant_share() {
        // A vivid subject on a large grey field: the coloured pixels are all
        // one hue, so it must still count as dominant.
        let sampled = sample_image(
            &image_of(&[(Rgb::rgb(128, 128, 128), 100), (Rgb::rgb(220, 30, 30), 20)]),
            Orientation::NoTransforms,
        );
        let result = extract_accent(&sampled, None);
        assert_eq!(result.source, AccentSource::DominantHue);
    }
}
