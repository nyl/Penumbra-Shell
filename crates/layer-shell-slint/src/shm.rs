//! Shared memory (SHM) buffer allocation and pixel layout helpers.
//!
//! Provides the [`Argb8888Pixel`] pixel adapter which implements Slint's
//! [`TargetPixel`], enabling
//! direct zero-copy software rendering into Wayland `wl_shm` slot buffers.

use slint::platform::software_renderer::{PremultipliedRgbaColor, TargetPixel};

/// Standard bytes-per-pixel for 32-bit ARGB8888 / XRGB8888 formats.
pub const BYTES_PER_PIXEL: u32 = 4;

/// Calculates the byte stride for an image row given a width in pixels.
#[inline]
pub fn calculate_stride(width: u32) -> i32 {
    (width * BYTES_PER_PIXEL) as i32
}

/// Calculates the total required byte size for a buffer given width and height.
#[inline]
pub fn calculate_buffer_size(width: u32, height: u32) -> usize {
    (width as usize) * (height as usize) * (BYTES_PER_PIXEL as usize)
}

/// A 32-bit pixel formatted as native little-endian ARGB8888 `[B, G, R, A]`.
///
/// In the Wayland `wl_shm` specification, `Format::Argb8888` is a 32-bit integer:
/// - Bits 24..31: Alpha
/// - Bits 16..23: Red
/// - Bits 8..15: Green
/// - Bits 0..7: Blue
///
/// On little-endian architectures (x86_64, aarch64), byte 0 is Blue and byte 3 is Alpha.
/// Implementing [`TargetPixel`] allows Slint's [`SoftwareRenderer`](slint::platform::software_renderer::SoftwareRenderer)
/// to render straight into Wayland SHM memory.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Argb8888Pixel {
    /// Blue channel (byte 0 in LE memory)
    pub b: u8,
    /// Green channel (byte 1 in LE memory)
    pub g: u8,
    /// Red channel (byte 2 in LE memory)
    pub r: u8,
    /// Alpha channel (byte 3 in LE memory)
    pub a: u8,
}

impl TargetPixel for Argb8888Pixel {
    /// Blends a premultiplied RGBA source color onto this destination pixel.
    #[inline]
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let a = color.alpha as u32;
        let inv_a = 255 - a;

        // Perform integer alpha blending: dst = src + dst * (1 - src_alpha)
        self.b = (color.blue as u32 + (self.b as u32 * inv_a + 127) / 255) as u8;
        self.g = (color.green as u32 + (self.g as u32 * inv_a + 127) / 255) as u8;
        self.r = (color.red as u32 + (self.r as u32 * inv_a + 127) / 255) as u8;
        self.a = (color.alpha as u32 + (self.a as u32 * inv_a + 127) / 255) as u8;
    }

    /// Constructs an opaque pixel from discrete RGB channels.
    #[inline]
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { b, g, r, a: 255 }
    }

    /// Pixel which will be filled as the background in case the slint view has transparency.
    #[inline]
    fn background() -> Self {
        Self { b: 0, g: 0, r: 0, a: 0 }
    }
}
