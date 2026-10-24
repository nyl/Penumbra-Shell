//! Android 17 / Material 3 28px squircle rounded corner mask rendering for application cards.
//!
//! Provides pre-computed anti-aliased corner patches and outline borders that overlay
//! Wayland application windows, smoothly rounding sharp client buffer edges.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
};

pub const CARD_CORNER_RADIUS: f64 = 28.0;
pub const CORNER_TILE_SIZE: i32 = 32;

/// Four pre-computed anti-aliased squircle corner masks for application cards.
pub struct CardCornerMasks {
    pub top_left: MemoryRenderBuffer,
    pub top_right: MemoryRenderBuffer,
    pub bottom_left: MemoryRenderBuffer,
    pub bottom_right: MemoryRenderBuffer,
}

impl Default for CardCornerMasks {
    fn default() -> Self {
        Self::new(CORNER_TILE_SIZE, CARD_CORNER_RADIUS)
    }
}

impl CardCornerMasks {
    pub fn new(size: i32, radius: f64) -> Self {
        let s = size as usize;
        let mut tl = vec![0u8; s * s * 4];
        let mut tr = vec![0u8; s * s * 4];
        let mut bl = vec![0u8; s * s * 4];
        let mut br = vec![0u8; s * s * 4];

        let scrim_r = 11u8;
        let scrim_g = 10u8;
        let scrim_b = 14u8;

        for y in 0..s {
            for x in 0..s {
                // Top-Left: center of curvature is at (size, size)
                let dx_tl = (size - 1 - x as i32) as f64;
                let dy_tl = (size - 1 - y as i32) as f64;
                let r_tl = (dx_tl * dx_tl + dy_tl * dy_tl).sqrt();
                write_pixel(&mut tl, s, x, y, r_tl, radius, scrim_r, scrim_g, scrim_b);

                // Top-Right: center of curvature is at (0, size)
                let dx_tr = x as f64;
                let dy_tr = (size - 1 - y as i32) as f64;
                let r_tr = (dx_tr * dx_tr + dy_tr * dy_tr).sqrt();
                write_pixel(&mut tr, s, x, y, r_tr, radius, scrim_r, scrim_g, scrim_b);

                // Bottom-Left: center of curvature is at (size, 0)
                let dx_bl = (size - 1 - x as i32) as f64;
                let dy_bl = y as f64;
                let r_bl = (dx_bl * dx_bl + dy_bl * dy_bl).sqrt();
                write_pixel(&mut bl, s, x, y, r_bl, radius, scrim_r, scrim_g, scrim_b);

                // Bottom-Right: center of curvature is at (0, 0)
                let dx_br = x as f64;
                let dy_br = y as f64;
                let r_br = (dx_br * dx_br + dy_br * dy_br).sqrt();
                write_pixel(&mut br, s, x, y, r_br, radius, scrim_r, scrim_g, scrim_b);
            }
        }

        let sz = (size, size);
        Self {
            top_left: MemoryRenderBuffer::from_slice(&tl, Fourcc::Abgr8888, sz, 1, Transform::Normal, None),
            top_right: MemoryRenderBuffer::from_slice(&tr, Fourcc::Abgr8888, sz, 1, Transform::Normal, None),
            bottom_left: MemoryRenderBuffer::from_slice(&bl, Fourcc::Abgr8888, sz, 1, Transform::Normal, None),
            bottom_right: MemoryRenderBuffer::from_slice(&br, Fourcc::Abgr8888, sz, 1, Transform::Normal, None),
        }
    }

    /// Generates 4 corner mask render elements positioned over the specified card bounds.
    pub fn card_elements(
        &self,
        renderer: &mut GlesRenderer,
        card_rect: Rectangle<i32, Physical>,
        scale: f64,
    ) -> Vec<MemoryRenderBufferRenderElement<GlesRenderer>> {
        let (cx, cy) = (card_rect.loc.x, card_rect.loc.y);
        let (cw, ch) = (card_rect.size.w, card_rect.size.h);
        let s = (CORNER_TILE_SIZE as f64 * scale).round() as i32;

        let corners = [
            ((cx, cy), &self.top_left),
            ((cx + cw - s, cy), &self.top_right),
            ((cx, cy + ch - s), &self.bottom_left),
            ((cx + cw - s, cy + ch - s), &self.bottom_right),
        ];

        let mut elements = Vec::with_capacity(4);
        for ((x, y), buf) in corners {
            let loc = Point::<f64, Physical>::from((x as f64, y as f64));
            if let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                loc,
                buf,
                None,
                None,
                Some(Size::<i32, Logical>::from((s, s))),
                Kind::Unspecified,
            ) {
                elements.push(elem);
            }
        }
        elements
    }
}

fn write_pixel(
    buf: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    dist: f64,
    radius: f64,
    scrim_r: u8,
    scrim_g: u8,
    scrim_b: u8,
) {
    let idx = (y * stride + x) * 4;
    if dist < radius - 1.5 {
        // Completely transparent inside the card
        buf[idx] = 0;
        buf[idx + 1] = 0;
        buf[idx + 2] = 0;
        buf[idx + 3] = 0;
    } else if dist <= radius + 0.5 {
        // Subtle white border highlight along the squircle perimeter
        let highlight = ((1.0 - (dist - radius).abs()) * 65.0).clamp(0.0, 65.0) as u8;
        buf[idx] = 255;
        buf[idx + 1] = 255;
        buf[idx + 2] = 255;
        buf[idx + 3] = highlight;
    } else {
        // Outside the card: anti-aliased dark scrim fill
        let edge_dist = dist - (radius + 0.5);
        let alpha = (edge_dist * 255.0).clamp(0.0, 255.0) as u8;
        buf[idx] = scrim_r;
        buf[idx + 1] = scrim_g;
        buf[idx + 2] = scrim_b;
        buf[idx + 3] = alpha;
    }
}
