//! Native image decoding and perceptual pixel sampling.
//!
//! The prototype shelled out to ImageMagick and flattened everything to 48×48
//! RGB. That loses the difference between a large flat sky and a small bright
//! subject, and needs a runtime dependency. Here the image is decoded in
//! process, downscaled with an area filter (which averages rather than picks
//! pixels, avoiding aliasing noise), and every sample keeps enough information
//! for the extractors: OKLCH coordinates plus a weight for area and saturation.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader};
use serde::{Deserialize, Serialize};

pub use image::metadata::Orientation as ImageOrientation;
use thiserror::Error;

use crate::color::Rgb;
use crate::oklab::Oklch;

/// Longest edge of the decoded thumbnail. 128 px is enough for stable colour
/// statistics while keeping a 4K wallpaper decode fast, and the area filter
/// makes the result independent of the source resolution.
pub const SAMPLE_EDGE: u32 = 128;

/// Total pixels sampled after downscaling, before filtering. A 128×72 image
/// yields about 9k samples, which is plenty for hue clustering.
const MAX_SAMPLES: usize = 16_384;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("cannot read image {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("wallpaper {path} has an unrecognised image format")]
    UnknownFormat { path: PathBuf },
    #[error("wallpaper {path} could not be decoded: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("wallpaper {path} is empty or fully transparent")]
    Empty { path: PathBuf },
    #[error("wallpaper {path} is {bytes} bytes, above the {limit} byte safety limit")]
    TooLarge {
        path: PathBuf,
        bytes: u64,
        limit: u64,
    },
}

/// Upper bound on a file we will decode, so a corrupt or hostile file cannot
/// make the service allocate wildly.
pub const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

/// A single sampled pixel, kept in perceptual coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub lch: Oklch,
    /// Area weight in `0.0..=1.0`, already multiplied by the saturation and
    /// lightness weighting used for accent selection.
    pub weight: f32,
}

/// A decoded wallpaper ready for palette extraction.
#[derive(Debug, Clone)]
pub struct SampledImage {
    pub width: u32,
    pub height: u32,
    pub samples: Vec<Sample>,
    /// Fraction of pixels that carry any visible colour, used to tell a real
    /// photo from an image that is mostly transparent or a flat field.
    pub opaque_fraction: f32,
    /// True when every visible pixel is within a hair of one hue and chroma,
    /// i.e. a monochrome or near-flat wallpaper.
    pub is_monochrome: bool,
}

/// Read the image orientation without decoding all pixels.
fn read_orientation(path: &Path) -> Option<Orientation> {
    let file = std::fs::File::open(path).ok()?;
    let reader = ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    ImageDecoder::orientation(&mut decoder).ok()
}

/// Decode and sample a wallpaper from disk.
pub fn sample_path(path: &Path) -> Result<SampledImage, ImageError> {
    let metadata = std::fs::metadata(path).map_err(|source| ImageError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ImageError::Read {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not a regular file"),
        });
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge {
            path: path.to_path_buf(),
            bytes: metadata.len(),
            limit: MAX_IMAGE_BYTES,
        });
    }
    if metadata.len() == 0 {
        return Err(ImageError::Empty {
            path: path.to_path_buf(),
        });
    }

    let file = std::fs::File::open(path).map_err(|source| ImageError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .map_err(|source| ImageError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if reader.format().is_none() {
        return Err(ImageError::UnknownFormat {
            path: path.to_path_buf(),
        });
    }
    let image = reader.decode().map_err(|source| ImageError::Decode {
        path: path.to_path_buf(),
        source,
    })?;

    let orientation = read_orientation(path).unwrap_or(Orientation::NoTransforms);
    Ok(sample_image(&image, orientation))
}

/// Sample an in-memory image, e.g. one already decoded for a preview.
pub fn sample_bytes(bytes: &[u8]) -> Result<SampledImage, ImageError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|source| ImageError::Read {
            path: PathBuf::from("<memory>"),
            source,
        })?;
    if reader.format().is_none() {
        return Err(ImageError::UnknownFormat {
            path: PathBuf::from("<memory>"),
        });
    }
    let image = reader.decode().map_err(|source| ImageError::Decode {
        path: PathBuf::from("<memory>"),
        source,
    })?;
    Ok(sample_image(&image, Orientation::NoTransforms))
}

/// Sample an already-decoded image. Exposed for tests and the GUI preview.
pub fn sample_image(image: &DynamicImage, orientation: Orientation) -> SampledImage {
    let mut image = image.clone();
    image.apply_orientation(orientation);

    let (width, height) = (image.width(), image.height());
    let thumb = image.thumbnail(SAMPLE_EDGE, SAMPLE_EDGE);
    let rgba = thumb.to_rgba8();
    let (tw, th) = rgba.dimensions();

    // Composite translucency over black, matching how Plasma draws a wallpaper
    // over the desktop background. Fully transparent pixels contribute nothing.
    let mut samples = Vec::with_capacity((tw * th) as usize);
    let mut visible = 0u32;
    let mut total = 0u32;
    let mut lch_min = Oklch {
        l: 1.0,
        c: f32::MAX,
        h: 0.0,
    };
    let mut lch_max_c = 0.0_f32;
    let mut hues = Vec::new();

    for pixel in rgba.pixels() {
        total += 1;
        let alpha = pixel[3];
        if alpha < 8 {
            continue;
        }
        visible += 1;
        // Scale the channel by alpha (straight alpha over black).
        let a = alpha as f32 / 255.0;
        let rgb = Rgb::rgb(
            (pixel[0] as f32 * a).round() as u8,
            (pixel[1] as f32 * a).round() as u8,
            (pixel[2] as f32 * a).round() as u8,
        );
        let lch = Oklch::from_rgb(rgb);

        // Weight: reward saturation and mid-to-high lightness so that a large
        // flat black border cannot outvote a small vivid subject, and so that
        // near-white highlights do not dominate by area alone.
        let saturation_factor = (lch.c / 0.18).clamp(0.0, 1.0);
        let lightness_factor = {
            let l = lch.l.clamp(0.0, 1.0);
            // Peaks around L≈0.6 and falls off towards both extremes.
            (1.0 - ((l - 0.6) / 0.6).abs()).clamp(0.0, 1.0) * 0.6 + 0.4
        };
        let weight = (0.15 + 0.85 * saturation_factor) * lightness_factor;

        if lch.c < lch_min.c && lch.l < lch_min.l {
            lch_min = lch;
        }
        lch_max_c = lch_max_c.max(lch.c);
        if lch.c > 0.02 {
            hues.push(lch.h);
        }
        samples.push(Sample { lch, weight });
    }

    if samples.len() > MAX_SAMPLES {
        // Keep an even stride rather than a prefix so every region of the image
        // is represented.
        let stride = samples.len().div_ceil(MAX_SAMPLES);
        samples = samples.into_iter().step_by(stride).collect();
    }

    let opaque_fraction = if total == 0 {
        0.0
    } else {
        visible as f32 / total as f32
    };
    let is_monochrome = is_monochrome_image(&samples, &hues);

    SampledImage {
        width,
        height,
        samples,
        opaque_fraction,
        is_monochrome,
    }
}

/// A wallpaper counts as monochrome when almost all coloured pixels sit in one
/// narrow hue band and chroma is low. Such an image still needs a usable
/// accent, so the caller substitutes a derived or configured fallback.
fn is_monochrome_image(samples: &[Sample], hues: &[f32]) -> bool {
    if samples.is_empty() {
        return true;
    }
    let coloured = hues.len() as f32 / samples.len().max(1) as f32;
    if coloured < 0.02 {
        // Almost every pixel is grey.
        return samples.iter().all(|s| s.lch.c < 0.04);
    }
    // Measure the hue spread on a 24-bucket histogram; one dominant bucket with
    // little elsewhere is still "effectively one colour".
    let mut buckets = [0u32; 24];
    for hue in hues {
        buckets[((hue / 15.0) as usize).min(23)] += 1;
    }
    let total: u32 = buckets.iter().sum();
    let dominant = *buckets.iter().max().unwrap_or(&0);
    (dominant as f32 / total.max(1) as f32) > 0.92
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
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
    use image::{RgbImage, RgbaImage};

    fn solid(r: u8, g: u8, b: u8, w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, image::Rgb([r, g, b])))
    }

    /// Two flat colour fields: half vivid blue, half vivid red.
    fn half_and_half(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = if x < w / 2 {
                image::Rgb([0, 0, 255])
            } else {
                image::Rgb([255, 0, 0])
            };
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn solid_images_are_reported_as_monochrome() {
        let sampled = sample_image(&solid(255, 0, 0, 64, 64), Orientation::NoTransforms);
        assert!(sampled.is_monochrome);
        assert_eq!(sampled.opaque_fraction, 1.0);
    }

    #[test]
    fn two_colour_images_are_not_monochrome() {
        let sampled = sample_image(&half_and_half(64, 64), Orientation::NoTransforms);
        assert!(!sampled.is_monochrome, "hue buckets were too concentrated");
    }

    #[test]
    fn greyscale_images_are_monochrome() {
        let mut img = RgbImage::new(64, 64);
        for (x, _, px) in img.enumerate_pixels_mut() {
            let v = (x * 4) as u8;
            *px = image::Rgb([v, v, v]);
        }
        let sampled = sample_image(&DynamicImage::ImageRgb8(img), Orientation::NoTransforms);
        assert!(sampled.is_monochrome);
    }

    #[test]
    fn fully_transparent_images_yield_no_samples() {
        let img = RgbaImage::from_pixel(32, 32, image::Rgba([255, 0, 0, 0]));
        let sampled = sample_image(&DynamicImage::ImageRgba8(img), Orientation::NoTransforms);
        assert!(sampled.samples.is_empty());
        assert_eq!(sampled.opaque_fraction, 0.0);
        assert!(sampled.is_monochrome);
    }

    #[test]
    fn half_transparent_pixels_are_composited_over_black() {
        let img = RgbaImage::from_pixel(16, 16, image::Rgba([255, 255, 255, 128]));
        let sampled = sample_image(&DynamicImage::ImageRgba8(img), Orientation::NoTransforms);
        // 128/255 white over black lands near mid grey, not white.
        let l = sampled.samples[0].lch.l;
        assert!(l < 0.75, "expected mid lightness, got {l}");
        assert!((sampled.opaque_fraction - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn downsizing_is_resolution_independent() {
        let small = sample_image(&half_and_half(64, 64), Orientation::NoTransforms);
        let large = sample_image(&half_and_half(1024, 1024), Orientation::NoTransforms);
        assert_eq!(small.samples.len(), large.samples.len());
        let avg_small: f32 =
            small.samples.iter().map(|s| s.lch.l).sum::<f32>() / small.samples.len() as f32;
        let avg_large: f32 =
            large.samples.iter().map(|s| s.lch.l).sum::<f32>() / large.samples.len() as f32;
        assert!(
            (avg_small - avg_large).abs() < 0.03,
            "{avg_small} vs {avg_large}"
        );
    }

    #[test]
    fn missing_file_is_reported_not_panicked() {
        let err = sample_path(Path::new("/nonexistent/wallpaper.png")).unwrap_err();
        assert!(matches!(err, ImageError::Read { .. }), "got {err}");
    }

    #[test]
    fn empty_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.png");
        std::fs::write(&path, b"").unwrap();
        assert!(matches!(
            sample_path(&path).unwrap_err(),
            ImageError::Empty { .. }
        ));
    }

    #[test]
    fn garbage_data_is_rejected_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.png");
        std::fs::write(&path, b"this is not an image at all").unwrap();
        let err = sample_path(&path).unwrap_err();
        assert!(
            matches!(
                err,
                ImageError::UnknownFormat { .. } | ImageError::Decode { .. }
            ),
            "got {err}"
        );
    }

    #[test]
    fn truncated_png_is_rejected_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.png");
        let mut bytes = Vec::new();
        solid(200, 100, 50, 128, 128)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(sample_path(&path).is_err());
    }

    #[test]
    fn png_and_jpeg_both_decode() {
        let dir = tempfile::tempdir().unwrap();
        for (name, format) in [
            ("wallpaper.png", image::ImageFormat::Png),
            ("wallpaper.jpg", image::ImageFormat::Jpeg),
            ("wallpaper.webp", image::ImageFormat::WebP),
        ] {
            let path = dir.path().join(name);
            let mut bytes = Vec::new();
            half_and_half(300, 200)
                .write_to(&mut Cursor::new(&mut bytes), format)
                .unwrap();
            std::fs::write(&path, bytes).unwrap();
            let sampled = sample_path(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!sampled.is_monochrome, "{name} should show two hues");
            assert_eq!((sampled.width, sampled.height), (300, 200));
        }
    }

    #[test]
    fn orientation_is_applied_to_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rotated.jpg");
        let mut bytes = Vec::new();
        solid(10, 20, 30, 300, 200)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(&path, bytes).unwrap();
        let sampled = sample_path(&path).unwrap();
        // No EXIF orientation in this file, so dimensions stay as written.
        assert_eq!((sampled.width, sampled.height), (300, 200));
    }
}
