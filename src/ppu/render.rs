//! Compositing the background and sprite pixel that land on one dot into one.

use super::*;

impl Ppu {
    /// Color index of the background at the current dot, and whether it is
    /// opaque (a pattern value of zero is transparent regardless of palette).
    fn background_pixel(&self) -> (u8, bool) {
        if self.mask & 0x08 == 0 {
            return (0, false);
        }
        // Fine X selects which of the sixteen shifted bits is on screen. One
        // shift of the whole word brings that bit of every lane down to the
        // bottom of its own lane.
        let shifted = self.bg_shifters >> (15 - self.x as u32);
        let lo = (shifted & 1) as u8;
        let hi = ((shifted >> 16) & 1) as u8;
        let pattern = (hi << 1) | lo;
        if pattern == 0 {
            return (0, false);
        }
        let pal_lo = ((shifted >> 32) & 1) as u8;
        let pal_hi = ((shifted >> 48) & 1) as u8;
        let palette = (pal_hi << 1) | pal_lo;
        (palette * 4 + pattern, true)
    }

    /// Color index of the frontmost opaque sprite at this dot, whether it
    /// draws behind the background, and whether it is sprite zero.
    ///
    /// Which sprites are over this dot is not a question of where they are:
    /// every sprite whose counter has run out is being drawn, and what it puts
    /// out is whatever its shifter has reached.
    fn sprite_pixel(&self) -> (u8, bool, bool) {
        if self.mask & 0x10 == 0 {
            return (0, false, false);
        }
        if self.sprite_live == 0 {
            return (0, false, false); // nothing has reached its first pixel
        }
        for i in 0..8 {
            if self.sprite_live & (1 << i) == 0 {
                continue; // still counting down, or already drawn out
            }
            let lo = (self.sprite_lo[i] >> 7) & 1;
            let hi = (self.sprite_hi[i] >> 7) & 1;
            let pattern = (hi << 1) | lo;
            if pattern == 0 {
                continue; // transparent: a lower-priority sprite may show
            }
            let attr = self.sprite_attr[i];
            let palette = 4 + (attr & 3);
            let behind = attr & 0x20 != 0;
            return (palette * 4 + pattern, behind, i == 0 && self.sprite_zero_loaded);
        }
        (0, false, false)
    }

    #[rustfmt::skip]
    pub(super) fn render_pixel(&mut self) {
        let x = self.dot - 1;
        let y = self.scanline as u16;

        // The leftmost eight pixels can be clipped independently per layer.
        let clip_bg = x < 8 && self.mask & 0x02 == 0;
        let clip_sp = x < 8 && self.mask & 0x04 == 0;

        let (bg_index, bg_opaque) = if clip_bg { (0, false) } else { self.background_pixel() };
        let (sp_index, sp_behind, is_zero) =
            if clip_sp { (0, false, false) } else { self.sprite_pixel() };
        let sp_opaque = sp_index != 0;

        // Sprite zero hit needs both layers opaque, and never fires on the
        // last dot of the line.
        if is_zero && bg_opaque && sp_opaque && x != 255 && self.rendering() {
            self.zero_hit_pending = true;
        }

        let index = match (bg_opaque, sp_opaque) {
            (false, false) => 0, // backdrop
            (false, true) => sp_index,
            (true, false) => bg_index,
            (true, true) => {
                if sp_behind { bg_index } else { sp_index }
            }
        };

        // Greyscale, the palette's mirrors and the emphasis bits are already
        // in the table, so this is the color as the PPU emits it.
        self.color_ids[y as usize * FRAME_WIDTH + x as usize] =
            self.emitted[index as usize & 0x1f];
    }
}

/// Apply the `$2001` color emphasis bits: each set bit attenuates the other
/// two channels by roughly 20%.
pub(super) fn emphasize(rgb: u32, bits: u8) -> u32 {
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let att = |c: u32| c * 4 / 5;
    let (mut r, mut g, mut b) = (r, g, b);
    if bits & 1 != 0 {
        g = att(g);
        b = att(b);
    }
    if bits & 2 != 0 {
        r = att(r);
        b = att(b);
    }
    if bits & 4 != 0 {
        r = att(r);
        g = att(g);
    }
    (r << 16) | (g << 8) | b
}
