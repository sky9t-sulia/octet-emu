//! The background pipeline: shift registers fed one tile fetch at a time.

use super::*;

impl Ppu {
    pub(super) fn shift_background(&mut self) {
        self.bg_shift_lo <<= 1;
        self.bg_shift_hi <<= 1;
        self.at_shift_lo <<= 1;
        self.at_shift_hi <<= 1;
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
    pub(super) fn background_fetch(&mut self, mapper: &mut dyn Mapper) {
        match self.dot & 7 {
            1 => {
                let addr = 0x2000 | (self.v & 0x0fff);
                self.nt_latch = self.read_vram(addr, mapper);
                // Latch the pattern address with the table select and fine Y
                // as they stand now; a later $2000 write cannot move it.
                self.tile_addr = ((self.nt_latch as u16) << 4)
                    | (self.v >> 12)
                    | if self.ctrl & 0x10 != 0 { 0x1000 } else { 0 };
            }
            3 => {
                // Each attribute byte covers a 4x4 tile area, and which of its
                // four 2-bit fields applies depends on bit 1 of coarse X and Y.
                let addr =
                    0x23c0 | (self.v & 0x0c00) | ((self.v >> 4) & 0x38) | ((self.v >> 2) & 0x07);
                let byte = self.read_vram(addr, mapper);
                let shift = ((self.v >> 4) & 4) | (self.v & 2);
                self.at_latch = (byte >> shift) & 3;
            }
            5 => {
                let addr = self.tile_addr;
                self.bg_lo_latch = self.read_vram(addr, mapper);
            }
            7 => {
                let addr = self.tile_addr + 8;
                self.bg_hi_latch = self.read_vram(addr, mapper);
            }
            // Last dot: advance the scroll and reload the shifters. Doing it
            // here rather than a dot later keeps background and sprites aligned.
            0 => {
                self.increment_x();
                self.reload_shifters();
            }
            _ => {}
        }
    }
}
