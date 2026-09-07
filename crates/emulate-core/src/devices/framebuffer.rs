//! RAM-backed scanout with page-generation tracking for dirty rows.

use crate::bus::DRAM_BASE;

/// Boot width, in pixels.
pub const FB_WIDTH: u32 = 1024;
/// Boot height, in pixels.
pub const FB_HEIGHT: u32 = 768;
/// Fixed backing stride; each scanline spans two RAM pages.
pub const FB_STRIDE: u32 = 8192;
pub const FB_MAX_WIDTH: u32 = 2048;
pub const FB_MAX_HEIGHT: u32 = 2048;
pub const FB_PAGES_PER_ROW: usize = FB_STRIDE as usize / 4096;
/// Bytes per pixel; the format is `x8r8g8b8`, i.e. B,G,R,X in memory order.
pub const FB_BYTES_PER_PIXEL: u32 = 4;
/// Backing storage for the largest supported mode.
pub const FB_BYTES: u64 = FB_STRIDE as u64 * FB_MAX_HEIGHT as u64;

/// Reserved scanout RAM, excluded from the guest allocator.
pub const FB_RESERVED_BYTES: u64 = 16 * 1024 * 1024;

/// Total DRAM of the machines that carry a display. The framebuffer is carved
/// out of the top of this, and the DTB's `memory` node is shrunk to match.
pub const FB_MACHINE_RAM_BYTES: u64 = 128 * 1024 * 1024;

/// Guest physical address of pixel (0, 0): the top of DRAM, less the reserved
/// window: 0x8700_0000.
pub const FB_BASE: u64 = DRAM_BASE + FB_MACHINE_RAM_BYTES - FB_RESERVED_BYTES;

/// The `format` string of the `simple-framebuffer` node
/// (`include/linux/platform_data/simplefb.h`).
pub const FB_FORMAT: &str = "x8r8g8b8";

/// Offset of the framebuffer inside DRAM.
pub const FB_RAM_OFFSET: u64 = FB_BASE - DRAM_BASE;

/// Index of the RAM page holding row 0.
pub const FB_FIRST_PAGE: u64 = FB_RAM_OFFSET / 4096;

/// Bytes one row occupies in a host RGBA buffer.
pub const FB_ROW_RGBA_BYTES: usize = (FB_WIDTH * FB_BYTES_PER_PIXEL) as usize;

/// A host's view of which framebuffer rows have changed.
///
/// Holds the write generation each row had the last time the host was told
/// about it. A generation of [`u64::MAX`] is the "never seen" sentinel — a
/// real page generation cannot reach it — so a fresh tracker, or one that has
/// been [`FramebufferView::mark_all_dirty`]ed after a snapshot restore,
/// reports every row once.
pub struct FramebufferView {
    seen: Vec<u64>,
}

impl Default for FramebufferView {
    fn default() -> Self {
        Self::new()
    }
}

impl FramebufferView {
    pub fn new() -> Self {
        Self {
            seen: vec![u64::MAX; FB_MAX_HEIGHT as usize],
        }
    }

    /// Forget everything, so the next scan reports all [`FB_HEIGHT`] rows.
    /// Used after a snapshot restore, where the RAM underneath was replaced
    /// wholesale and the host's canvas holds the wrong picture.
    pub fn mark_all_dirty(&mut self) {
        self.seen.fill(u64::MAX);
    }

    /// Append the rows whose generation changed since the last scan to `out`,
    /// in ascending order, and adopt the new generations.
    ///
    /// Each generation combines the pages in one active scanline.
    pub fn scan(&mut self, generations: &[u64], out: &mut Vec<u32>) {
        for (row, (&generation, seen)) in generations.iter().zip(&mut self.seen).enumerate() {
            if generation != *seen {
                *seen = generation;
                out.push(row as u32);
            }
        }
    }
}

/// Convert one `x8r8g8b8` scanline (B,G,R,X in memory) into RGBA, the byte
/// order a canvas `ImageData` wants.
///
/// Done on the host rather than in the guest: `simpledrm`'s native plane
/// format is XRGB8888; host conversion avoids guest software conversion.
pub fn row_to_rgba(src: &[u8], dst: &mut [u8]) {
    let pixels = src.len().min(dst.len()) / 4;
    if src.len() < pixels * 4 || dst.len() < pixels * 4 {
        return;
    }
    for (pixel, out) in src[..pixels * 4]
        .chunks_exact(4)
        .zip(dst.chunks_exact_mut(4))
    {
        out[0] = pixel[2];
        out[1] = pixel[1];
        out[2] = pixel[0];
        out[3] = 0xff;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_is_self_consistent_and_page_aligned() {
        assert_eq!(FB_STRIDE, FB_MAX_WIDTH * FB_BYTES_PER_PIXEL);
        assert_eq!(FB_STRIDE as usize, 4096 * FB_PAGES_PER_ROW);
        assert_eq!(FB_BASE, 0x8700_0000);
        assert_eq!(FB_BYTES, 16 * 1024 * 1024);
        const { assert!(FB_BYTES <= FB_RESERVED_BYTES) };
        assert_eq!(FB_RAM_OFFSET % FB_STRIDE as u64, 0);
    }

    #[test]
    fn a_fresh_view_reports_every_row_once() {
        let mut view = FramebufferView::new();
        let generations = vec![0u64; FB_HEIGHT as usize];
        let mut rows = Vec::new();

        view.scan(&generations, &mut rows);
        assert_eq!(rows.len(), FB_HEIGHT as usize);
        assert_eq!(rows[0], 0);
        assert_eq!(rows[FB_HEIGHT as usize - 1], FB_HEIGHT - 1);

        rows.clear();
        view.scan(&generations, &mut rows);
        assert!(rows.is_empty());
    }

    #[test]
    fn only_changed_rows_are_reported() {
        let mut view = FramebufferView::new();
        let mut generations = vec![0u64; FB_HEIGHT as usize];
        let mut rows = Vec::new();
        view.scan(&generations, &mut rows);
        rows.clear();

        generations[7] = 1;
        generations[200] = 9;
        view.scan(&generations, &mut rows);

        assert_eq!(rows, vec![7, 200]);
    }

    #[test]
    fn mark_all_dirty_forces_a_full_repaint() {
        let mut view = FramebufferView::new();
        let generations = vec![3u64; FB_HEIGHT as usize];
        let mut rows = Vec::new();
        view.scan(&generations, &mut rows);
        rows.clear();

        view.mark_all_dirty();
        view.scan(&generations, &mut rows);

        assert_eq!(rows.len(), FB_HEIGHT as usize);
    }

    #[test]
    fn xrgb_becomes_opaque_rgba() {
        let mut src = vec![0u8; FB_STRIDE as usize];
        // Blue pixel: B=0xff, G=0, R=0, X=0.
        src[0] = 0xff;
        // Red pixel at x=1: B=0, G=0, R=0xff.
        src[6] = 0xff;
        let mut dst = vec![0u8; FB_ROW_RGBA_BYTES];

        row_to_rgba(&src, &mut dst);

        assert_eq!(&dst[..4], &[0x00, 0x00, 0xff, 0xff]);
        assert_eq!(&dst[4..8], &[0xff, 0x00, 0x00, 0xff]);
    }
}
