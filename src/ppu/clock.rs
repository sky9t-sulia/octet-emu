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
        if self.pending_any {
            self.apply_write(mapper);
        }

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

            // A line is a fixed sequence of stages, and a dot belongs to
            // exactly one of them. Asking which one in a single jump beats
            // walking a list of ranges, most of which any given dot fails.
            match self.dot {
                // Idle. The sprite unit sits reading secondary OAM at
                // wherever its address came to rest.
                0 => self.oam_buffer = self.peek_secondary_oam(),

                // Secondary OAM is cleared a byte every two dots, over the
                // first sixty-four of a visible line. The pre-render line does
                // not do it, which is what leaves last frame's sprites to be
                // drawn on line 0.
                1..=64 => {
                    if visible {
                        self.clear_secondary_oam();
                    }
                    // The clear ends here, and hands the memory to
                    // evaluation: the address goes back to the start and
                    // whatever pinned it is let go.
                    if self.dot == 63 {
                        self.oam2_full = false;
                        self.oam2_addr = 0;
                    }
                    self.fetch_dot(mapper, visible, true);
                }

                // The same fetch, with evaluation running alongside it.
                65..=256 => {
                    self.fetch_dot(mapper, visible, true);
                    if self.dot == 255 {
                        self.oam2_full = false;
                    }
                    if self.dot == 256 {
                        self.increment_y();
                    }
                    // Evaluation only happens on visible lines. The pre-render
                    // line neither clears secondary OAM nor looks at OAM at
                    // all, so its fetch loads the shifters from whatever the
                    // last visible line left behind — and those sprites are
                    // drawn on scanline 0.
                    if visible {
                        if self.dot == 65 {
                            self.start_sprite_evaluation();
                        }
                        // Two dots to a step: OAM is read on the odd one,
                        // secondary OAM written on the even one.
                        if self.dot % 2 == 1 {
                            self.sprite_eval_read();
                        } else {
                            self.sprite_eval_write();
                        }
                    }
                }

                // Evaluation hands over to the sprite fetch, which therefore
                // starts reading at the first sprite.
                257 => {
                    self.oam2_addr = 0;
                    self.prepare_sprites();
                    // The dummy nametable read this dot goes out on the
                    // address the line was left with, not the one the scroll
                    // is about to be reset to -- the reset lands after it.
                    self.sprite_fetch(mapper);
                    self.copy_x();
                    self.oam_addr = 0;
                }

                // The fetch itself. The vertical scroll is restored across a
                // window inside it, repeatedly rather than just once.
                258..=320 => {
                    if pre_render && self.dot >= 280 && self.dot <= 304 {
                        self.copy_y();
                    }
                    self.sprite_fetch(mapper);
                    self.oam_addr = 0;
                }

                // The first two tiles of the next line, fetched ahead.
                321..=336 => {
                    self.fetch_dot(mapper, false, false);
                    self.oam_buffer = self.peek_secondary_oam();
                }

                // Two redundant nametable fetches end the line. MMC5 counts
                // them to work out where it is in the frame.
                _ => {
                    if self.dot == 339 {
                        self.oam2_full = false;
                        self.oam2_addr = 0;
                    }
                    if self.dot == 337 || self.dot == 339 {
                        let addr = 0x2000 | (self.v & 0x0fff);
                        self.nt_latch = self.read_vram(addr, mapper);
                    }
                    // The second of them is also where every counter is told
                    // to count again. Rendering has to be on for that to
                    // happen: if it is not, a sprite that was being drawn
                    // stays that way, and is drawn again from wherever
                    // rendering resumes.
                    if self.dot == 339 {
                        self.restart_sprite_counters();
                    }
                    self.oam_buffer = self.peek_secondary_oam();
                }
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

        // A12 is sampled once per dot, not once per fetch, but the cartridge
        // only ever acts on a rise -- about once a tile -- so the dots it
        // spends low are counted here and handed over with the rise.
        if self.mapper_watches_bus {
            let high = self.bus_addr & 0x1000 != 0;
            if high {
                if !self.a12_level {
                    mapper.a12_rose(self.a12_low_dots);
                }
                self.a12_low_dots = 0;
            } else {
                self.a12_low_dots = self.a12_low_dots.saturating_add(1);
            }
            self.a12_level = high;
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
