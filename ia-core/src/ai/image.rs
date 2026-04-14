//! Image processing for AI QA — crop, rotate, and resize page images.
//!
//! Matches the behavior of the extraction system's `ImageProcessor`:
//! 1. Rotate by `rotateDegree` from scandata (negated, matching PIL convention)
//! 2. Crop to `cropBox` from scandata
//! 3. Resize to cap the longest side (our addition for QA cost savings)
//! 4. Encode as JPEG quality 85

use std::io::Cursor;

use image::imageops::FilterType;
use image::DynamicImage;

use crate::error::{IaError, Result};
use crate::scandata::{CropBox, ScandataPage};

/// Image quality presets for QA vision requests.
///
/// Controls the maximum pixel dimension (longest side) of page images sent
/// to the LLM. Smaller images use fewer tokens and cost less, but may lose
/// fine detail. Anthropic charges `(width × height) / 750` tokens per image
/// and auto-resizes anything above 1568px.
///
/// Approximate costs per image with Claude Sonnet (8 pages typical):
///
/// | Level | Max px | ~DPI | Tokens/img | 8 images  | Best for                    |
/// |-------|--------|------|------------|-----------|-----------------------------|
/// | high  | 1568   | 143  | ~1,548     | ~$0.048   | Fine print, handwriting     |
/// | medium| 1024   | 93   | ~1,040     | ~$0.034   | Standard printed text (default) |
/// | low   | 768    | 70   | ~588       | ~$0.023   | Large print, titles only    |
/// | min   | 512    | 47   | ~260       | ~$0.012   | Covers, simple verification |
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImageQuality {
    /// 1568px — full Anthropic resolution, no savings
    High,
    /// 1024px — good for standard printed text (default)
    #[default]
    Medium,
    /// 768px — adequate for large print and titles
    Low,
    /// 512px — minimal, covers and simple fields only
    Min,
}

impl ImageQuality {
    /// Maximum pixel dimension (longest side) for this quality level.
    #[must_use]
    pub fn max_dimension(self) -> u32 {
        match self {
            Self::High => 1568,
            Self::Medium => 1024,
            Self::Low => 768,
            Self::Min => 512,
        }
    }

    /// Estimated tokens per image at this quality level (for a typical book page).
    ///
    /// Calibrated from real Anthropic API responses on ~1577×1988 derivative
    /// JP2s. Anthropic's formula is `(w × h) / 750` but their server-side
    /// resize behavior means actual counts differ from naive calculation.
    #[must_use]
    pub fn est_tokens_per_image(self) -> u64 {
        match self {
            Self::High => 1548,
            Self::Medium => 1040,
            Self::Low => 588,
            Self::Min => 260,
        }
    }
}

impl std::fmt::Display for ImageQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
            Self::Min => write!(f, "min"),
        }
    }
}

impl std::str::FromStr for ImageQuality {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "high" => Ok(Self::High),
            "medium" | "med" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            "min" | "minimum" => Ok(Self::Min),
            _ => Err(format!(
                "unknown image quality '{s}': expected high, medium, low, or min"
            )),
        }
    }
}

/// Default maximum dimension (longest side) for QA images.
pub const DEFAULT_MAX_DIMENSION: u32 = 1024;

/// Process a page image: rotate, crop, resize, and re-encode as JPEG.
///
/// Matches the extraction system's `ImageProcessor._convert_image_to_jpeg`:
/// - Rotate by scandata's `rotateDegree` (negated for PIL-compatible direction)
/// - Crop to scandata's `cropBox`
/// - Additionally resize for QA cost savings (not done by extractor)
///
/// Returns the processed JPEG bytes.
pub fn process_page_image(
    jpeg_bytes: &[u8],
    page: &ScandataPage,
    max_dimension: u32,
) -> Result<Vec<u8>> {
    let mut img = image::load_from_memory(jpeg_bytes).map_err(|e| {
        IaError::Config(format!(
            "failed to decode image for leaf {}: {e}",
            page.leaf_num
        ))
    })?;

    // 1. Crop FIRST — cropBox coordinates are in the original (pre-rotation) space.
    if let Some(crop) = &page.crop_box {
        img = apply_crop(img, crop);
    }

    // 2. Rotate — the extractor does PIL.rotate(-rotateDegree, expand=True).
    // PIL.rotate(angle) is counterclockwise, so rotate(-N) = N° clockwise.
    // rotateDegree=90 → rotate clockwise 90° → image crate's rotate90().
    img = apply_rotation(img, page.rotate_degree);

    // 3. Resize if larger than max_dimension
    img = apply_resize(img, max_dimension);

    // 4. Encode as JPEG
    encode_jpeg(&img)
}

/// Resize only — for derivative images that are already correctly oriented.
///
/// Derivative JP2s from `_jp2.zip` are already rotated and oriented by the
/// derive pipeline. We only need to resize for LLM cost savings.
pub fn resize_only(jpeg_bytes: &[u8], max_dimension: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory(jpeg_bytes)
        .map_err(|e| IaError::Config(format!("failed to decode image for resize: {e}")))?;

    let img = apply_resize(img, max_dimension);
    encode_jpeg(&img)
}

/// Apply rotation matching the extractor's convention.
///
/// The extractor does `PIL.rotate(-rotateDegree, expand=True)`.
/// PIL's `rotate(angle)` is counterclockwise, so `rotate(-N)` = N° clockwise.
///
/// - `rotateDegree=90`  → 90° clockwise  → `rotate90()`
/// - `rotateDegree=-90` → 90° counterclockwise → `rotate270()`
/// - `rotateDegree=180` → 180° → `rotate180()`
fn apply_rotation(img: DynamicImage, rotate_degree: i32) -> DynamicImage {
    // Normalize to 0-359 range
    let normalized = rotate_degree.rem_euclid(360);
    match normalized {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img, // 0 or non-standard angles — no rotation
    }
}

/// Crop to the scandata `cropBox`, clamping to image bounds.
fn apply_crop(img: DynamicImage, crop: &CropBox) -> DynamicImage {
    let (img_w, img_h) = (img.width(), img.height());

    // Clamp crop box to image dimensions
    let x = crop.x.min(img_w.saturating_sub(1));
    let y = crop.y.min(img_h.saturating_sub(1));
    let w = crop.w.min(img_w.saturating_sub(x));
    let h = crop.h.min(img_h.saturating_sub(y));

    if w == 0 || h == 0 {
        return img;
    }

    img.crop_imm(x, y, w, h)
}

/// Resize if the longest side exceeds `max_dimension`.
fn apply_resize(img: DynamicImage, max_dimension: u32) -> DynamicImage {
    if max_dimension == 0 {
        return img;
    }

    let (w, h) = (img.width(), img.height());
    let longest = w.max(h);

    if longest <= max_dimension {
        return img;
    }

    let scale = max_dimension as f64 / longest as f64;
    let new_w = (w as f64 * scale).round() as u32;
    let new_h = (h as f64 * scale).round() as u32;

    img.resize_exact(new_w, new_h, FilterType::Lanczos3)
}

/// Encode a `DynamicImage` as JPEG bytes.
fn encode_jpeg(img: &DynamicImage) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);

    img.write_to(&mut cursor, image::ImageFormat::Jpeg)
        .map_err(|e| IaError::Config(format!("failed to encode JPEG: {e}")))?;

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a simple test image (solid color) at given dimensions.
    fn test_image(width: u32, height: u32) -> Vec<u8> {
        let img = DynamicImage::new_rgb8(width, height);
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    fn test_page(leaf_num: u32, rotate: i32, crop: Option<CropBox>) -> ScandataPage {
        ScandataPage {
            leaf_num,
            page_type: "Normal".to_string(),
            crop_box: crop,
            rotate_degree: rotate,
            add_to_access_formats: true,
        }
    }

    #[test]
    fn no_rotation_no_crop_no_resize() {
        let jpeg = test_image(100, 100);
        let page = test_page(0, 0, None);
        let result = process_page_image(&jpeg, &page, 2000).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 100);
    }

    #[test]
    fn rotation_90_swaps_dimensions() {
        // rotateDegree=90 → 90° clockwise → 200×400 becomes 400×200
        let jpeg = test_image(200, 400);
        let page = test_page(0, 90, None);
        let result = process_page_image(&jpeg, &page, 2000).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        assert_eq!(img.width(), 400);
        assert_eq!(img.height(), 200);
    }

    #[test]
    fn rotation_negative_90_swaps_dimensions() {
        // rotateDegree=-90 → 90° counterclockwise → 200×400 becomes 400×200
        let jpeg = test_image(200, 400);
        let page = test_page(0, -90, None);
        let result = process_page_image(&jpeg, &page, 2000).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        assert_eq!(img.width(), 400);
        assert_eq!(img.height(), 200);
    }

    #[test]
    fn rotation_180() {
        let jpeg = test_image(200, 400);
        let page = test_page(0, 180, None);
        let result = process_page_image(&jpeg, &page, 2000).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        // Same dimensions after 180°
        assert_eq!(img.width(), 200);
        assert_eq!(img.height(), 400);
    }

    #[test]
    fn crop_applied() {
        let jpeg = test_image(1000, 1000);
        let crop = CropBox {
            x: 100,
            y: 200,
            w: 500,
            h: 300,
        };
        let page = test_page(0, 0, Some(crop));
        let result = process_page_image(&jpeg, &page, 2000).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        assert_eq!(img.width(), 500);
        assert_eq!(img.height(), 300);
    }

    #[test]
    fn crop_clamped_to_image_bounds() {
        let jpeg = test_image(500, 500);
        let crop = CropBox {
            x: 400,
            y: 400,
            w: 300, // extends beyond image
            h: 300,
        };
        let page = test_page(0, 0, Some(crop));
        let result = process_page_image(&jpeg, &page, 2000).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        // Should clamp: x=400, w=min(300, 500-400)=100, same for h
        assert_eq!(img.width(), 100);
        assert_eq!(img.height(), 100);
    }

    #[test]
    fn resize_caps_longest_side() {
        let jpeg = test_image(3000, 4000);
        let page = test_page(0, 0, None);
        let result = process_page_image(&jpeg, &page, 1536).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        // 3000×4000 → longest=4000, scale=1536/4000=0.384
        // new: 1152×1536
        assert_eq!(img.height(), 1536);
        assert!(img.width() <= 1536);
    }

    #[test]
    fn no_resize_when_already_small() {
        let jpeg = test_image(500, 700);
        let page = test_page(0, 0, None);
        let result = process_page_image(&jpeg, &page, 1536).unwrap();
        let img = image::load_from_memory(&result).unwrap();
        assert_eq!(img.width(), 500);
        assert_eq!(img.height(), 700);
    }

    #[test]
    fn full_pipeline_crop_rotate_resize() {
        // Simulate a real page: 4000×6000, crop in original coords, then rotate 90° CW
        let jpeg = test_image(4000, 6000);
        let crop = CropBox {
            x: 500,
            y: 1000,
            w: 3000,
            h: 4000,
        };
        let page = test_page(1, 90, Some(crop));
        let result = process_page_image(&jpeg, &page, 1536).unwrap();
        let img = image::load_from_memory(&result).unwrap();

        // Step 1 - Crop in original coords: 4000×6000 → 3000×4000
        // Step 2 - Rotate 90° CW: 3000×4000 → 4000×3000
        // Step 3 - Resize: longest=4000, scale=1536/4000=0.384 → 1536×1152
        assert!(img.width() <= 1536);
        assert!(img.height() <= 1536);
    }

    #[test]
    fn invalid_image_bytes_returns_error() {
        let page = test_page(0, 0, None);
        let result = process_page_image(b"not an image", &page, 1536);
        assert!(result.is_err());
    }
}
