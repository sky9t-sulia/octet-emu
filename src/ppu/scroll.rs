//! The v/t/x/w scroll counters -- the real loopy registers, not an abstraction of them.

use super::*;

impl Ppu {
    /// Advance the coarse X counter, flipping the horizontal nametable bit on
    /// wrap. `v` is laid out as `0yyy NNYY YYYX XXXX`.
    pub(super) fn increment_x(&mut self) {
        if self.v & 0x001f == 31 {
            self.v &= !0x001f;
            self.v ^= 0x0400;
        } else {
            self.v += 1;
        }
    }

    #[rustfmt::skip]
    pub(super) fn increment_y(&mut self) {
        if self.v & 0x7000 != 0x7000 {
            self.v += 0x1000; // still within the tile row
        } else {
            self.v &= !0x7000;
            let mut coarse_y = (self.v & 0x03e0) >> 5;
            if coarse_y == 29 {
                // Row 30 and 31 hold the attribute table, so the nametable
                // flips here rather than at 31.
                coarse_y = 0;
                self.v ^= 0x0800;
            } else if coarse_y == 31 { coarse_y = 0; } else { coarse_y += 1; }
            self.v = (self.v & !0x03e0) | (coarse_y << 5);
        }
    }

    pub(super) fn copy_x(&mut self) {
        self.v = (self.v & !0x041f) | (self.t & 0x041f);
    }

    pub(super) fn copy_y(&mut self) {
        self.v = (self.v & !0x7be0) | (self.t & 0x7be0);
    }
}
