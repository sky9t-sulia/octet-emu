//! Sprite evaluation: which of the next scanline's sprites make the cut, two per dot.

use super::*;

impl Ppu {
    #[rustfmt::skip]
    fn sprite_height(&self) -> u16 {
        if self.ctrl & 0x20 != 0 { 16 } else { 8 }
    }

    /// Begin sprite evaluation for the next scanline. Secondary OAM reads back
    /// as $FF for the first 64 dots while it is cleared.
    pub(super) fn start_sprite_evaluation(&mut self) {
        self.secondary_oam = [0xff; 32];
        self.eval_n = 0;
        self.eval_m = 0;
        self.eval_found = 0;
        self.eval_done = false;
        self.sprite_zero_next = false;
    }

    /// One step of sprite evaluation, two dots each across 65-256, which is
    /// why overflow appears partway through a scanline.
    pub(super) fn step_sprite_evaluation(&mut self) {
        if self.eval_done || self.eval_n >= 64 {
            self.eval_done = true;
            return;
        }
        let height = self.sprite_height();
        let line = self.scanline as u16;
        let in_range = |y: u8| {
            let y = y as u16;
            line >= y && line < y + height
        };

        if self.eval_found < 8 {
            if self.eval_m == 0 {
                let y = self.oam[self.eval_n * 4];
                if in_range(y) {
                    // In range: spend the next three steps copying the rest.
                    if self.eval_n == 0 {
                        self.sprite_zero_next = true;
                    }
                    self.secondary_oam[self.eval_found * 4] = y;
                    self.eval_m = 1;
                } else {
                    self.eval_n += 1;
                }
            } else {
                self.secondary_oam[self.eval_found * 4 + self.eval_m] =
                    self.oam[self.eval_n * 4 + self.eval_m];
                self.eval_m += 1;
                if self.eval_m == 4 {
                    self.eval_m = 0;
                    self.eval_found += 1;
                    self.eval_n += 1;
                }
            }
        } else {
            // Eight found, so this pass only looks for overflow. The byte
            // index advances too — the bug that tests non-Y bytes as Y.
            if in_range(self.oam[self.eval_n * 4 + self.eval_m]) {
                self.status |= 0x20;
                self.eval_done = true;
            } else {
                self.eval_n += 1;
                self.eval_m = (self.eval_m + 1) & 3;
            }
        }
    }

    /// Work out each selected sprite's pattern address; the fetches happen a
    /// dot at a time in [`Ppu::sprite_fetch`], since mappers time their A12.
    pub(super) fn prepare_sprites(&mut self) {
        let height = self.sprite_height();
        let line = self.scanline as u16;
        // Evaluation has finished by now, so its results become the set of
        // sprites the next scanline will draw.
        self.sprite_count = self.eval_found;
        self.sprite_zero_loaded = self.sprite_zero_next;

        for i in 0..8 {
            let y = self.secondary_oam[i * 4] as u16;
            let tile = self.secondary_oam[i * 4 + 1];
            let attr = self.secondary_oam[i * 4 + 2];
            let x = self.secondary_oam[i * 4 + 3];

            let mut row = line.wrapping_sub(y) & (height - 1);
            if attr & 0x80 != 0 {
                row = height - 1 - row; // vertical flip
            }

            self.sprite_addr[i] = if height == 16 {
                // Tall sprites take their pattern table from bit 0 of the tile
                // index, and the lower half follows the upper.
                let table = ((tile & 1) as u16) << 12;
                let tile = (tile & 0xfe) as u16 + if row >= 8 { 1 } else { 0 };
                table + tile * 16 + (row & 7)
            } else {
                let table = if self.ctrl & 0x08 != 0 { 0x1000 } else { 0 };
                table + (tile as u16) * 16 + (row & 7)
            };
            self.sprite_attr[i] = attr;
            self.sprite_x[i] = x;
        }
    }

    /// One dot of the sprite fetch phase, 257-320. Eight dots per sprite: four
    /// garbage reads, then the two pattern reads that matter.
    pub(super) fn sprite_fetch(&mut self, mapper: &mut dyn Mapper) {
        let step = self.dot - 257;
        let i = (step / 8) as usize;
        match step % 8 {
            // The garbage fetches are real bus activity at the current
            // nametable address, and mappers can see them.
            0 | 2 => {
                let addr = 0x2000 | (self.v & 0x0fff);
                self.read_vram(addr, mapper);
            }
            4 => {
                let addr = self.sprite_addr[i];
                let mut lo = self.read_vram(addr, mapper);
                if self.sprite_attr[i] & 0x40 != 0 {
                    lo = lo.reverse_bits(); // horizontal flip
                }
                // Slots past the end of the sprite list still fetch, but the
                // data is discarded rather than drawn.
                self.sprite_lo[i] = if i < self.sprite_count { lo } else { 0 };
            }
            6 => {
                let addr = self.sprite_addr[i] + 8;
                let mut hi = self.read_vram(addr, mapper);
                if self.sprite_attr[i] & 0x40 != 0 {
                    hi = hi.reverse_bits();
                }
                self.sprite_hi[i] = if i < self.sprite_count { hi } else { 0 };
            }
            _ => {}
        }
    }
}
