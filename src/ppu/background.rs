//! The background pipeline: shift registers fed one tile fetch at a time.

use super::*;

impl Ppu {
    pub(super) fn shift_background(&mut self) {
        // A one comes in at the bottom of the pattern registers, not a zero:
        // nothing drives their serial input and it floats high. Invisible
        // while tiles keep arriving, since a reload overwrites the low byte
        // every eight dots — it only shows when a reload is skipped.
        self.bg_shift_lo = (self.bg_shift_lo << 1) | 1;
        self.bg_shift_hi = (self.bg_shift_hi << 1) | 1;
        self.at_shift_lo <<= 1;
        self.at_shift_hi <<= 1;
    }

    /// What a dot inside a fetch window does: the pixel it can draw, the two
    /// sets of shifters, and its share of the eight-dot tile fetch.
    ///
    /// `draw` is off on the pre-render line, which fetches without drawing,
    /// and `sprites` is off for the two tiles fetched ahead at the end of a
    /// line, where the sprite shifters have already been reloaded.
    #[inline(always)]
    pub(super) fn fetch_dot(&mut self, mapper: &mut dyn Mapper, draw: bool, sprites: bool) {
        if draw {
            self.render_pixel();
        }
        self.shift_background();
        if sprites {
            self.shift_sprites();
        }
        self.background_fetch(mapper);
    }

    /// Move the freshly fetched tile into the low half of the shift registers.
    fn reload_shifters(&mut self) {
        self.bg_shift_lo = (self.bg_shift_lo & 0xff00) | self.bg_lo_latch as u16;
        self.bg_shift_hi = (self.bg_shift_hi & 0xff00) | self.bg_hi_latch as u16;
        // The two attribute bits are expanded to eight so they shift alongside
        // the pattern bits.
        self.at_shift_lo =
            (self.at_shift_lo & 0xff00) | if self.at_latch & 1 != 0 { 0xff } else { 0 };
        self.at_shift_hi =
            (self.at_shift_hi & 0xff00) | if self.at_latch & 2 != 0 { 0xff } else { 0 };
    }

    /// One step of the eight-dot background fetch cycle.
    /// Where the nametable byte for the tile being fetched lives.
    fn nametable_addr(&self) -> u16 {
        0x2000 | (self.v & 0x0fff)
    }

    /// Where its attribute byte lives.
    fn attribute_addr(&self) -> u16 {
        0x23c0 | (self.v & 0x0c00) | ((self.v >> 4) & 0x38) | ((self.v >> 2) & 0x07)
    }

    pub(super) fn background_fetch(&mut self, mapper: &mut dyn Mapper) {
        // Each of the four fetches takes two dots: the address goes out on
        // the first and the read happens on the second.
        match self.dot & 7 {
            1 => {
                let addr = self.nametable_addr();
                self.ale(addr);
            }
            2 => {
                let driven = self.nametable_addr();
                self.nt_latch = self.read_latched(driven, mapper);
                // Latch the pattern address with the table select and fine Y
                // as they stand now; a later $2000 write cannot move it.
                self.tile_addr = ((self.nt_latch as u16) << 4)
                    | (self.v >> 12)
                    | if self.ctrl & 0x10 != 0 { 0x1000 } else { 0 };
            }
            3 => {
                let addr = self.attribute_addr();
                self.ale(addr);
            }
            4 => {
                let driven = self.attribute_addr();
                let byte = self.read_latched(driven, mapper);
                // Each attribute byte covers a 4x4 tile area, and which of its
                // four 2-bit fields applies depends on bit 1 of coarse X and Y.
                let shift = ((self.v >> 4) & 4) | (self.v & 2);
                self.at_latch = (byte >> shift) & 3;
            }
            5 => {
                let addr = self.tile_addr;
                self.ale(addr);
            }
            6 => {
                let driven = self.tile_addr;
                self.bg_lo_latch = self.read_latched(driven, mapper);
            }
            7 => {
                let addr = self.tile_addr + 8;
                self.ale(addr);
            }
            // Last dot: the second half of the pattern arrives, then the
            // scroll advances and the shifters reload. Doing that here rather
            // than a dot later keeps background and sprites aligned.
            0 => {
                let driven = self.tile_addr + 8;
                self.bg_hi_latch = self.read_latched(driven, mapper);
                self.increment_x();
                self.reload_shifters();
            }
            _ => {}
        }
    }
}
