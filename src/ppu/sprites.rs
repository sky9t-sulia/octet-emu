//! Sprite evaluation: which of the next Scanline's sprites make the cut, two per dot.

use super::*;

impl Ppu {
    #[rustfmt::skip]
    fn sprite_height(&self) -> u16 {
        if self.ctrl & 0x20 != 0 { 16 } else { 8 }
    }

    /// One dot of the clear that opens a visible line: a byte of secondary OAM
    /// every two dots, its address walking along with them. Nothing reads that
    /// address here, but [`Ppu::corrupt_oam`] takes it if rendering stops.
    pub(super) fn clear_secondary_oam(&mut self) {
        self.oam2_addr = ((self.dot - 1) >> 1) as u8;
        self.oam_buffer = 0xff;
        self.secondary_oam[self.oam2_addr as usize] = 0xff;
    }

    /// The damage done by switching rendering off while the sprite unit was
    /// working: the row of OAM the secondary address had reached is overwritten
    /// by OAM's first row. It waits for rendering to come back before it lands,
    /// which is why a frame can look untouched and the next one not.
    pub(super) fn corrupt_oam(&mut self) {
        let Some(row) = self.oam_corrupt_row.take() else {
            return;
        };
        let base = (row as usize & 0x1f) * 8;
        for i in 0..8 {
            self.oam[base + i] = self.oam[i];
        }
    }

    /// Begin sprite evaluation for the next scanline. Secondary OAM has read
    /// back as $FF since the clear above went through it.
    pub(super) fn start_sprite_evaluation(&mut self) {
        self.eval_start = self.oam_addr;
        self.eval_overflow_pending = false;
        self.eval_extra = 0;
        self.oam2_addr = 0;
        self.oam2_full = false;
        self.eval_n = 0;
        self.eval_m = 0;
        self.eval_done = false;
        self.sprite_zero_next = false;
    }

    /// Move the secondary OAM address on by one.
    ///
    /// Running off the end latches [`Ppu::oam2_full`], and a latched flag
    /// pins the address: it cannot be moved again until evaluation starts
    /// over. Page 22's `Frozen OAM2 Increment` is about exactly that.
    pub(super) fn oam2_bump(&mut self) {
        if self.oam2_full {
            return;
        }
        self.oam2_addr = (self.oam2_addr + 1) & 0x1f;
        if self.oam2_addr == 0 {
            self.oam2_full = true;
        }
    }

    /// Where in OAM evaluation is looking. Every read is relative to the
    /// address evaluation started from, and wraps.
    fn eval_addr(&self) -> usize {
        (self.eval_start as usize + self.eval_n * 4 + self.eval_m) & 0xff
    }

    /// The read half of an evaluation step, on the odd dots: a byte of OAM
    /// into the buffer. Nothing else happens on these dots.
    pub(super) fn sprite_eval_read(&mut self) {
        // A sprite found on the step before reaches the flag now.
        if self.eval_overflow_pending {
            self.eval_overflow_pending = false;
            self.status |= 0x20;
        }

        let addr = self.eval_addr();
        self.oam_buffer = self.oam[addr];
        // Three of the bits of an attribute byte have no memory behind them.
        if addr & 3 == 2 {
            self.oam_buffer &= 0xe3;
        }
        // A ninth sprite on the line is found as its byte arrives, not a dot
        // later when the unit gives up trying to keep it.
        if self.oam2_full && self.eval_extra == 0 && !self.eval_done {
            let y = self.oam_buffer as u16;
            let line = self.scanline as u16;
            if line >= y && line < y + self.sprite_height() {
                self.eval_overflow_pending = true;
            }
        }
    }

    /// The 'write' half, on the even dots: the byte in hand goes to secondary
    /// OAM, and evaluation decides where to look next.
    ///
    /// When the unit is not allowed to write — secondary OAM full, or every
    /// sprite already looked at — secondary OAM is read instead, and the
    /// buffer takes that. It is why `$2004` reads a rejected sprite's Y over
    /// and over for the rest of the line.
    pub(super) fn sprite_eval_write(&mut self) {
        let height = self.sprite_height();
        let line = self.scanline as u16;
        let y = self.oam_buffer as u16;
        let in_range = line >= y && line < y + height;

        let slot = (self.oam2_addr & 0x1f) as usize;
        if self.oam2_full || self.eval_done {
            self.oam_buffer = self.secondary_oam[slot];
        } else {
            // The Y goes in wherever the address points whether the
            // sprite is wanted; only a wanted one moves the address on, so the
            // next sprite's Y lands on top of a rejected one.
            self.secondary_oam[slot] = self.oam_buffer;
        }

        if self.eval_done {
            // Every sprite has been looked at, and the unit spends the rest of
            // the line reading one Y after another and failing to keep them.
            self.eval_n = (self.eval_n + 1) & 63;
            self.eval_m = 0;
        } else if !self.oam2_full {
            if self.eval_m == 0 {
                if in_range {
                    // In range: spend the next three steps copying the rest.
                    // Sprite zero is the first sprite evaluation looks at,
                    // which is the one the starting address points to — not
                    // whichever happens to sit at index zero.
                    if self.eval_n == 0 {
                        self.sprite_zero_next = true;
                    }
                    self.oam2_bump();
                    self.eval_m = 1;
                } else {
                    self.eval_n += 1;
                }
            } else {
                self.oam2_bump();
                self.eval_m += 1;
                if self.eval_m == 4 {
                    self.eval_m = 0;
                    self.eval_n += 1;
                }
            }
            if self.eval_n >= 64 {
                self.eval_n = 0;
                self.eval_done = true;
            }
        } else if self.eval_extra > 0 {
            // Reading the ninth sprite out, a byte a step. The last of them
            // comes back to the byte it started at, and then the unit is done
            // with the line: from here it only counts sprites past.
            self.eval_extra -= 1;
            match self.eval_extra {
                3 => self.eval_m = 1,
                2 => self.eval_m = 2,
                1 => self.eval_m = 0,
                _ => {
                    self.eval_n = (self.eval_n + 1) & 63;
                    self.eval_m = 0;
                    self.eval_done = true;
                }
            }
        } else {
            // Secondary OAM is full, so this pass only looks for overflow. The
            // byte index advances with the sprite — the bug that tests a
            // sprite's other bytes as if they were its Y.
            if in_range {
                // The comparison lands a step late, by which time the address
                // has moved on a sprite — and it is that one the unit reads
                // out before giving up on the line.
                self.eval_n = (self.eval_n + 1) & 63;
                self.eval_m = 0;
                self.eval_extra = 4;
            } else {
                self.eval_n += 1;
                self.eval_m = (self.eval_m + 1) & 3;
                if self.eval_n >= 64 {
                    self.eval_n = 0;
                    self.eval_done = true;
                }
            }
        }

        // The address register is what evaluation walks OAM with, so it ends
        // up wherever evaluation has got to — which a `$2004` read taken
        // mid-scanline can see moving.
        self.oam_addr = self.eval_addr() as u8;
    }

    /// The sprites of the line about to be drawn take over from the ones being
    /// drawn now. Everything else about them — their bytes, their patterns —
    /// is read a dot at a time in [`Ppu::sprite_fetch`].
    pub(super) fn prepare_sprites(&mut self) {
        self.sprite_zero_loaded = self.sprite_zero_next;
    }

    /// Take the byte secondary OAM's address points at, without moving the
    /// address. It goes through the buffer, as everything the unit reads does.
    fn oam2_peek(&mut self) -> u8 {
        self.oam_buffer = self.secondary_oam[(self.oam2_addr & 0x1f) as usize];
        self.oam_buffer
    }

    /// The same as oam2_peek but then the address moves on. An address something has
    /// pinned gives the same byte every time.
    fn oam2_read(&mut self) -> u8 {
        let val = self.oam2_peek();
        self.oam2_bump();
        val
    }

    /// The line the sprite fetch compares against, which is not always the
    /// line being drawn: the counter reads 261 on the pre-render line and only
    /// eight bits of it are kept, making it line 5. That is why a sprite left
    /// behind in secondary OAM is fetched at row 5 and drawn on scanline 0,
    /// which page 22's `Sprites On Scanline 0` is about.
    fn fetch_line(&self) -> u16 {
        if self.scanline < 0 {
            261 & 0xff
        } else {
            self.scanline as u16
        }
    }

    /// Whether the sprite the fetch is on falls on that line at all. It is the
    /// same comparison evaluation made, made again — and an empty slot, whose
    /// Y is $FF, is what it usually turns away.
    fn sprite_in_range(&self, i: usize) -> bool {
        let y = self.sprite_y[i] as u16;
        let line = self.fetch_line();
        line >= y && line < y + self.sprite_height()
    }

    /// Where the row of a sprite's pattern that lands on this line lives,
    /// from the three bytes the fetch has read of it.
    fn sprite_pattern_addr(&self, i: usize) -> u16 {
        let height = self.sprite_height();
        let y = self.sprite_y[i] as u16;
        let tile = self.sprite_tile[i];

        let mut row = self.fetch_line().wrapping_sub(y) & (height - 1);
        if self.sprite_attr[i] & 0x80 != 0 {
            row = height - 1 - row; // vertical flip
        }

        if height == 16 {
            // Tall sprites take their pattern table from bit 0 of the tile
            // index, and the lower half follows the upper.
            let table = ((tile & 1) as u16) << 12;
            let tile = (tile & 0xfe) as u16 + if row >= 8 { 1 } else { 0 };
            table + tile * 16 + (row & 7)
        } else {
            let table = if self.ctrl & 0x08 != 0 { 0x1000 } else { 0 };
            table + (tile as u16) * 16 + (row & 7)
        }
    }

    /// Keep the drawn-and-not-empty mask true after a pattern is loaded into a
    /// sprite that is already past its counter.
    fn note_sprite_pattern(&mut self, i: usize) {
        let bit = 1 << i;
        let live = self.sprite_counting & bit == 0 && self.sprite_lo[i] | self.sprite_hi[i] != 0;
        if live {
            self.sprite_live |= bit;
        } else {
            self.sprite_live &= !bit;
        }
    }

    /// One dot of the eight shifter counters. A counter that has run out puts
    /// its sprite into the halted state, which is where it is drawn; until
    /// then it only counts, and forced blank does not stop it counting.
    ///
    /// All eight count together and none of them is ever loaded while they are
    /// counting, so rather than step eight numbers down a dot at a time, this
    /// steps one number up and compares it against the nearest position still
    /// waiting. A line has 256 counting dots and at most eight arrivals on it,
    /// so all but eight of them end at that comparison.
    pub(super) fn step_sprite_counters(&mut self) {
        if self.sprite_counting == 0 {
            return;
        }
        if self.sprite_soonest == SPRITE_UNKNOWN {
            self.find_soonest_sprite();
        }
        self.sprite_elapsed += 1;
        if self.sprite_elapsed <= self.sprite_soonest {
            return;
        }

        // Something is due. Everything at or behind the count arrives at once:
        // sprites sharing a position, and any left behind by a fetch that did
        // not reload them.
        for i in 0..8 {
            let bit = 1 << i;
            if self.sprite_counting & bit == 0 {
                continue;
            }
            if self.sprite_counter[i] as u16 >= self.sprite_elapsed {
                continue;
            }
            self.sprite_counting &= !bit;
            if self.sprite_lo[i] | self.sprite_hi[i] != 0 {
                self.sprite_live |= bit;
            }
        }
        self.sprite_soonest = SPRITE_UNKNOWN;
    }

    /// Where the nearest sprite still on its way is going to arrive.
    fn find_soonest_sprite(&mut self) {
        let mut soonest = SPRITE_UNKNOWN;
        for i in 0..8 {
            if self.sprite_counting & (1 << i) == 0 {
                continue;
            }
            let at = self.sprite_counter[i] as u16;
            if at < soonest {
                soonest = at;
            }
        }
        self.sprite_soonest = soonest;
    }

    /// Take a sprite's position off the bus as the fetch reads it. Every
    /// counter is measured from the moment the last one was loaded, so loading
    /// one starts the measurement over.
    fn load_sprite_counter(&mut self, i: usize, x: u8) {
        self.sprite_counter[i] = x;
        self.sprite_elapsed = 0;
        self.sprite_soonest = SPRITE_UNKNOWN;
    }

    /// Whatever byte of secondary OAM the unit's address is resting on. Read
    /// over and over while nothing else is using the memory.
    #[inline(always)]
    pub(super) fn peek_secondary_oam(&self) -> u8 {
        self.secondary_oam[(self.oam2_addr & 0x1f) as usize]
    }

    /// Set all eight counting again at the end of a line.
    pub(super) fn restart_sprite_counters(&mut self) {
        self.sprite_counting = 0xff;
        self.sprite_live = 0;
        self.sprite_soonest = SPRITE_UNKNOWN;
    }

    /// The pattern shifters of every sprite being drawn, one place along.
    /// Nothing shifts in behind them, so a sprite runs out after its eight
    /// pixels instead of repeating — and stops where it is if rendering does.
    pub(super) fn shift_sprites(&mut self) {
        if self.sprite_live == 0 {
            return;
        }
        for i in 0..8 {
            let bit = 1 << i;
            if self.sprite_live & bit == 0 {
                continue;
            }
            self.sprite_lo[i] <<= 1;
            self.sprite_hi[i] <<= 1;
            if self.sprite_lo[i] | self.sprite_hi[i] == 0 {
                self.sprite_live &= !bit;
            }
        }
    }

    /// One dot of the sprite fetch phase, 257-320. Eight dots per sprite: four
    /// garbage reads, then the two pattern reads that matter.
    pub(super) fn sprite_fetch(&mut self, mapper: &mut dyn Mapper) {
        let step = self.dot - 257;
        let i = (step / 8) as usize;
        match step % 8 {
            // A sprite's four bytes come out of secondary OAM, one every two
            // dots — so the fetch walks the same address the copying did, and
            // an address something has pinned hands every sprite the same
            // byte four times over.
            //
            // The garbage fetches on the first four dots are real bus activity
            // at the current nametable address, and mappers can see them.
            0 => {
                self.sprite_y[i] = self.oam2_read();
                let addr = 0x2000 | (self.v & 0x0fff);
                self.read_vram(addr, mapper);
            }
            1 => self.sprite_tile[i] = self.oam2_read(),
            2 => {
                self.sprite_attr[i] = self.oam2_read();
                let addr = 0x2000 | (self.v & 0x0fff);
                self.read_vram(addr, mapper);
            }
            // The X position is read over and over across the rest of the
            // sprite's dots, and only the last of them moves the address on.
            // The counter takes it each time, so a fetch cut short leaves
            // whatever the line before put there.
            3 | 5 => {
                let x = self.oam2_peek();
                self.load_sprite_counter(i, x);
            }
            4 => {
                let x = self.oam2_peek();
                self.load_sprite_counter(i, x);
                self.sprite_addr[i] = self.sprite_pattern_addr(i);
                let addr = self.sprite_addr[i];
                let mut lo = self.read_vram(addr, mapper);
                if self.sprite_attr[i] & 0x40 != 0 {
                    lo = lo.reverse_bits(); // horizontal flip
                }
                // A slot whose sprite is not on this line is still fetched —
                // the read is out on the bus and mappers count it — but what
                // reaches the shifter is nothing at all.
                self.sprite_lo[i] = if self.sprite_in_range(i) { lo } else { 0 };
                self.note_sprite_pattern(i);
            }
            6 => {
                let x = self.oam2_peek();
                self.load_sprite_counter(i, x);
                let addr = self.sprite_addr[i] + 8;
                let mut hi = self.read_vram(addr, mapper);
                if self.sprite_attr[i] & 0x40 != 0 {
                    hi = hi.reverse_bits();
                }
                self.sprite_hi[i] = if self.sprite_in_range(i) { hi } else { 0 };
                self.note_sprite_pattern(i);
            }
            7 => {
                let x = self.oam2_read();
                self.load_sprite_counter(i, x);
            }
            _ => {}
        }
    }
}
