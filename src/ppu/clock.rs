//! The dot clock: what happens on each of the 341 dots of each of the 262 scanlines.

use super::*;

impl Ppu {
    /// Advance by one CPU cycle's worth of dots.
    pub fn tick(&mut self, mapper: &mut dyn Mapper) {
        // NTSC and Dendy run three dots per CPU cycle; PAL runs 3.2, which the
        // accumulator spreads out as a repeating 3,3,3,3,4 pattern.
        let dots = if self.tv == TvSystem::Pal {
            self.dot_accum += 2;
            if self.dot_accum >= 10 {
                self.dot_accum -= 10;
                4
            } else {
                3
            }
        } else {
            3
        };
        for _ in 0..dots {
            self.dot_tick(mapper);
        }
    }

    /// Advance a single dot, for a bus that interleaves the CPU access at a
    /// chosen point inside the three-dot window.
    pub fn tick_dot(&mut self, mapper: &mut dyn Mapper) {
        self.dot_tick(mapper);
    }

    fn dot_tick(&mut self, mapper: &mut dyn Mapper) {
        let pre_render = self.scanline == -1;
        let visible = self.scanline >= 0 && self.scanline < 240;

        if pre_render && self.dot == 1 {
            // Vblank, sprite zero hit and overflow all clear together.
            self.status &= !0xe0;
        }

        if self.rendering() && (visible || pre_render) {
            if (self.dot >= 1 && self.dot <= 256) || (self.dot >= 321 && self.dot <= 336) {
                if visible && self.dot <= 256 {
                    self.render_pixel();
                }
                self.shift_background();
                self.background_fetch(mapper);
            } else if self.dot == 257 {
                self.copy_x();
            } else if self.dot == 337 || self.dot == 339 {
                // Two redundant nametable fetches end the line. MMC5 counts
                // them to work out where it is in the frame.
                let addr = 0x2000 | (self.v & 0x0fff);
                self.nt_latch = self.read_vram(addr, mapper);
            }

            if self.dot == 256 {
                self.increment_y();
            }
            // The vertical scroll is restored repeatedly across this window,
            // not just once.
            if pre_render && self.dot >= 280 && self.dot <= 304 {
                self.copy_y();
            }

            // Evaluation only happens on visible lines; the pre-render line
            // clears secondary OAM but finds nothing.
            if visible {
                if self.dot == 65 {
                    self.start_sprite_evaluation();
                } else if self.dot > 65 && self.dot <= 256 && self.dot % 2 == 1 {
                    self.step_sprite_evaluation();
                }
            }
            if self.dot == 257 {
                if !visible {
                    // The pre-render line evaluates nothing.
                    self.start_sprite_evaluation();
                }
                self.prepare_sprites();
            }
            if self.dot >= 257 && self.dot <= 320 {
                self.sprite_fetch(mapper);
            }
            // OAM address is cleared throughout sprite fetching.
            if self.dot >= 257 && self.dot <= 320 {
                self.oam_addr = 0;
            }
        } else if visible && self.dot >= 1 && self.dot <= 256 {
            // With rendering off the backdrop is still drawn — and if `v`
            // happens to point into the palette, that entry shows instead.
            let index = if (self.v & 0x3f00) == 0x3f00 { (self.v & 0x1f) as u8 } else { 0 };
            let color = self.read_palette(0x3f00 + index as u16) as usize;
            let x = (self.dot - 1) as usize;
            self.framebuffer[self.scanline as usize * FRAME_WIDTH + x] = PALETTE[color & 0x3f];
        }

        if self.scanline == self.vblank_scanline() && self.dot == 1 {
            if !self.vbl_suppress {
                self.status |= 0x80;
            }
            self.vbl_suppress = false;
            self.frame += 1;
        }

        if self.need_state_update {
            self.update_state();
        }

        // The mapper samples A12 once per dot, not once per fetch.
        mapper.tick_ppu_addr(self.bus_addr);
        self.advance_dot();
    }

    fn advance_dot(&mut self) {
        self.dot += 1;
        // On odd frames with rendering enabled, the pre-render line is one dot
        // short, which is what keeps the NTSC colour phase alternating.
        let line_length = if self.scanline == -1
            && self.odd_frame
            && self.rendering()
            && self.tv == TvSystem::Ntsc
        {
            DOTS_PER_SCANLINE - 1
        } else {
            DOTS_PER_SCANLINE
        };

        if self.dot >= line_length {
            self.dot = 0;
            self.scanline += 1;
            if self.scanline > self.last_scanline() {
                self.scanline = -1;
                self.odd_frame = !self.odd_frame;
            }
        }
    }
}
