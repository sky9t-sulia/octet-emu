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
        // Whatever the processor wrote on the dot just gone is felt now.
        self.apply_write(mapper);

        let pre_render = self.scanline == -1;
        let visible = self.scanline >= 0 && self.scanline < 240;

        if pre_render && self.dot == 1 {
            // Vblank, sprite zero hit and overflow all clear together.
            self.status &= !0xe0;
        }

        // The comparison the dot just gone made, reaching the flag.
        if self.zero_hit_pending {
            self.status |= 0x40;
            self.zero_hit_pending = false;
        }

        // The shifter counters run on every dot a pixel could come out of,
        // whether one does: forced blank stops the shifting below, and
        // the fetch that reloads them, but never the counting.
        if self.dot >= 1 && self.dot <= 256 {
            self.step_sprite_counters();
        }

        if self.rendering() && (visible || pre_render) {
            // Rendering coming back is when OAM pays for having had it taken
            // away mid-line.
            self.corrupt_oam();

            // Secondary OAM is cleared a byte every two dots, over the first
            // sixty-four of a visible line. The pre-render line does not do it,
            // which is what leaves last frame's sprites to be drawn on line 0.
            if visible && self.dot >= 1 && self.dot <= 64 {
                self.clear_secondary_oam();
            }

            // The secondary OAM address goes back to the start of its memory
            // three times a line: at the end of the clear, the end of
            // evaluation, and the end of the line. Whatever pinned it is let
            // go at the same moments. Rendering has to be on as each goes
            // by, which is how the address can still be frozen, or left part
            // way along, when the next stage starts reading.
            if matches!(self.dot, 63 | 255 | 339) {
                self.oam2_full = false;
            }
            // The address itself goes back to the start of secondary OAM as
            // each stage -- hands over to the next — the last of which is the
            // fetch, which therefore starts reading at the first sprite.
            if matches!(self.dot, 63 | 257 | 339) {
                self.oam2_addr = 0;
            }

            if (self.dot >= 1 && self.dot <= 256) || (self.dot >= 321 && self.dot <= 336) {
                if visible && self.dot <= 256 {
                    self.render_pixel();
                }
                self.shift_background();
                if self.dot <= 256 {
                    self.shift_sprites();
                }
                self.background_fetch(mapper);
            } else if self.dot == 257 {
                self.copy_x();
            } else if self.dot == 337 || self.dot == 339 {
                // Two redundant nametable fetches end the line. MMC5 counts
                // them to work out where it is in the frame.
                let addr = 0x2000 | (self.v & 0x0fff);
                self.nt_latch = self.read_vram(addr, mapper);
                // The second of them is also where every counter is told to
                // count again. Rendering has to be on for that to happen: if
                // it is not, a sprite that was being drawn stays that way, and
                // is drawn again from wherever rendering resumes.
                if self.dot == 339 {
                    self.sprite_counting = 0xff;
                    self.sprite_live = 0;
                }
            }

            if self.dot == 256 {
                self.increment_y();
            }
            // The vertical scroll is restored repeatedly across this window,
            // not just once.
            if pre_render && self.dot >= 280 && self.dot <= 304 {
                self.copy_y();
            }

            // Evaluation only happens on visible lines. The pre-render line
            // neither clears secondary OAM nor looks at OAM at all, so its
            // fetch loads the shifters from whatever the last visible line
            // left behind — and those sprites are drawn on scanline 0.
            if visible && (65..=256).contains(&self.dot) {
                if self.dot == 65 {
                    self.start_sprite_evaluation();
                }
                // Two dots to a step: OAM is read on the odd one, secondary
                // OAM written on the even one.
                if self.dot % 2 == 1 {
                    self.sprite_eval_read();
                } else {
                    self.sprite_eval_write();
                }
            }
            if self.dot == 257 {
                self.prepare_sprites();
            }
            if self.dot >= 257 && self.dot <= 320 {
                self.sprite_fetch(mapper);
            }
            // Between the fetch ending and the next line's clear, the unit
            // sits reading secondary OAM at wherever its address came to rest.
            if self.dot == 0 || self.dot >= 321 {
                self.oam_buffer = self.secondary_oam[(self.oam2_addr & 0x1f) as usize];
            }

            // OAM address is cleared throughout sprite fetching.
            if self.dot >= 257 && self.dot <= 320 {
                self.oam_addr = 0;
            }
        } else if visible && self.dot >= 1 && self.dot <= 256 {
            // With rendering off the backdrop is still drawn — and if `v`
            // happens to point into the palette, that entry shows instead.
            let index = if (self.v & 0x3f00) == 0x3f00 { (self.v & 0x1f) as u8 } else { 0 };
            let color = self.read_palette(0x3f00 + index as u16) as usize & 0x3f;
            let x = (self.dot - 1) as usize;
            // No emphasis here: with rendering off, `$2001`'s tint bits do
            // not reach the backdrop the way they do a rendered pixel.
            self.color_ids[self.scanline as usize * FRAME_WIDTH + x] = color as u16;
        }

        if self.scanline == self.vblank_scanline() && self.dot == 1 {
            if !self.vbl_suppress {
                self.status |= 0x80;
            }
            self.vbl_suppress = false;
            self.frame += 1;
            self.decay_bus();
        }

        if self.need_state_update {
            self.update_state();
        }

        // The mapper samples A12 once per dot, not once per fetch.
        // A `$2007` read taken while rendering fills the buffer here, two
        // dots on, with whatever the bus carried in the meantime.
        if self.buffer_fill > 0 {
            self.buffer_fill -= 1;
            if self.buffer_fill == 0 {
                self.read_buffer = self.bus_data;
            }
        }

        if self.mapper_watches_bus {
            mapper.tick_ppu_addr(self.bus_addr);
        }
        self.advance_dot();
    }

    fn advance_dot(&mut self) {
        self.dot += 1;
        // On odd frames with rendering enabled, the pre-render line is one dot
        // short, which is what keeps the NTSC color phase alternating.
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
