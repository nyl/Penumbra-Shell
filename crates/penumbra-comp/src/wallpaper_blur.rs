//! Pre-blurred wallpaper caching and dual-kawase filtering for Penumbra Mobile DE.
//!
//! # Architecture & Pipeline
//! To achieve 60/120 FPS continuous gestures without GPU stall during window scaling,
//! the homescreen wallpaper is loaded, downsampled by 1/4 resolution, and pre-filtered
//! with dual-kawase blur passes at compositor startup.
//!
//! During interactive swipe-to-overview or spring settling:
//! 1. The pre-blurred wallpaper is composited with opacity modulated by gesture progress $t$.
//! 2. A Material 3 dimming scrim (`#0b0a0e`, max 45% alpha) is blended over the wallpaper.
//! 3. The transforming application window card is composited on top with 28px rounded corners.

use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Default candidate wallpaper paths searched in priority order.
const WALLPAPER_CANDIDATES: &[&str] = &[
    "/usr/share/penumbra/assets/wallpaper.jpg",
    "assets/wallpaper.jpg",
    "/usr/share/wallpapers/penumbra/wallpaper.jpg",
    "/usr/share/wallpapers/default.jpg",
    "/usr/share/backgrounds/default.png",
];

/// Target downscale factor for wallpaper blur pre-caching (1/4 resolution).
const DOWNSCALE_FACTOR: u32 = 4;

/// In-memory pre-blurred wallpaper buffer ready for zero-cost GLES texture upload.
#[derive(Debug, Clone)]
pub struct BlurredWallpaperCache {
    /// RGBA8888 raw pixel byte buffer.
    pub data: Vec<u8>,
    /// Logical target width in pixels (e.g. 412).
    pub logical_width: i32,
    /// Logical target height in pixels (e.g. 915).
    pub logical_height: i32,
    /// Downscaled buffer width in pixels (e.g. 103).
    pub buffer_width: u32,
    /// Downscaled buffer height in pixels (e.g. 228).
    pub buffer_height: u32,
}

impl BlurredWallpaperCache {
    /// Locates and loads the active homescreen wallpaper, downscaling by 1/4 and
    /// applying dual-kawase blur passes.
    ///
    /// # Arguments
    /// * `custom_path` - Optional explicit path to wallpaper image file.
    /// * `logical_w` - Screen logical width (e.g. 412).
    /// * `logical_h` - Screen logical height (e.g. 915).
    ///
    /// # Returns
    /// A new `BlurredWallpaperCache` instance containing the blurred pixel buffer,
    /// or a fallback dark gradient buffer if loading failed.
    ///
    /// # Mathematical Formulation
    /// Downscale resolution:
    /// $$W_{\text{down}} = \max(1, \lfloor W_{\text{logical}} / 4 \rfloor)$$
    /// $$H_{\text{down}} = \max(1, \lfloor H_{\text{logical}} / 4 \rfloor)$$
    pub fn load_and_blur(
        custom_path: Option<&str>,
        logical_w: i32,
        logical_h: i32,
    ) -> Self {
        let down_w = (logical_w as u32 / DOWNSCALE_FACTOR).max(1);
        let down_h = (logical_h as u32 / DOWNSCALE_FACTOR).max(1);

        if let Some(path_str) = custom_path {
            let path = Path::new(path_str);
            if path.exists() {
                if let Ok(cache) = Self::process_image(path, logical_w, logical_h, down_w, down_h) {
                    return cache;
                }
            }
        }

        for candidate in WALLPAPER_CANDIDATES {
            let path = PathBuf::from(candidate);
            if path.exists() {
                if let Ok(cache) = Self::process_image(&path, logical_w, logical_h, down_w, down_h) {
                    info!("Successfully pre-cached blurred wallpaper from {}", candidate);
                    return cache;
                }
            }
        }

        warn!("No wallpaper found; generating fallback blurred gradient buffer");
        Self::create_fallback(logical_w, logical_h, down_w, down_h)
    }

    /// Processes an image from disk: decodes, downsamples to 1/4, and applies dual-kawase blur.
    fn process_image(
        path: &Path,
        logical_w: i32,
        logical_h: i32,
        down_w: u32,
        down_h: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let img = image::open(path)?;
        let rgba = img.to_rgba8();

        // 1. Downscale to 1/4 resolution using triangle filter for fast downsampling
        let downsampled = image::imageops::resize(
            &rgba,
            down_w,
            down_h,
            image::imageops::FilterType::Triangle,
        );

        // 2. Dual-kawase blur passes
        let blurred = apply_dual_kawase(&downsampled, 3);

        Ok(Self {
            data: blurred.into_raw(),
            logical_width: logical_w,
            logical_height: logical_h,
            buffer_width: down_w,
            buffer_height: down_h,
        })
    }

    /// Creates a fallback dark Material 3 blurred gradient if no image file is found.
    fn create_fallback(logical_w: i32, logical_h: i32, down_w: u32, down_h: u32) -> Self {
        let mut data = Vec::with_capacity((down_w * down_h * 4) as usize);
        for y in 0..down_h {
            let t = y as f64 / down_h as f64;
            // Dark purple/indigo gradient #0e0d16 -> #161224
            let r = (14.0 * (1.0 - t) + 22.0 * t).round() as u8;
            let g = (13.0 * (1.0 - t) + 18.0 * t).round() as u8;
            let b = (22.0 * (1.0 - t) + 36.0 * t).round() as u8;
            for _ in 0..down_w {
                data.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Self {
            data,
            logical_width: logical_w,
            logical_height: logical_h,
            buffer_width: down_w,
            buffer_height: down_h,
        }
    }
}

/// Applies multi-iteration dual-kawase blur passes to an RGBA image buffer.
///
/// # Arguments
/// * `src` - Input downscaled image buffer.
/// * `iterations` - Number of blur kernel iterations (typically 2-4).
///
/// # Returns
/// Filtered `image::RgbaImage` with smooth Gaussian-like dispersion.
///
/// # Mathematical Formulation
/// Dual-kawase evaluates diagonal cross samples offset by distance $d$:
/// $$C(x, y) = \frac{1}{4} \sum_{i \in \{-d, d\}, j \in \{-d, d\}} I(x + i, y + j)$$
/// Successive passes increase offset $d = 1, 2, 3$ to disperse high-frequency detail.
fn apply_dual_kawase(src: &image::RgbaImage, iterations: usize) -> image::RgbaImage {
    let width = src.width();
    let height = src.height();
    let mut current = src.clone();

    for pass in 0..iterations {
        let offset = (pass + 1) as i32;
        let mut next = image::RgbaImage::new(width, height);

        for y in 0..height {
            for x in 0..width {
                let x_i = x as i32;
                let y_i = y as i32;

                let offsets = [
                    (-offset, -offset),
                    (offset, -offset),
                    (-offset, offset),
                    (offset, offset),
                ];

                let mut acc_r = 0u32;
                let mut acc_g = 0u32;
                let mut acc_b = 0u32;
                let mut acc_a = 0u32;

                for (dx, dy) in offsets {
                    let sx = (x_i + dx).clamp(0, width as i32 - 1) as u32;
                    let sy = (y_i + dy).clamp(0, height as i32 - 1) as u32;
                    let pixel = current.get_pixel(sx, sy);
                    acc_r += pixel[0] as u32;
                    acc_g += pixel[1] as u32;
                    acc_b += pixel[2] as u32;
                    acc_a += pixel[3] as u32;
                }

                let out_pixel = image::Rgba([
                    (acc_r / 4) as u8,
                    (acc_g / 4) as u8,
                    (acc_b / 4) as u8,
                    (acc_a / 4) as u8,
                ]);
                next.put_pixel(x, y, out_pixel);
            }
        }
        current = next;
    }

    current
}
