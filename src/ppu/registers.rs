//! CPU-facing register reads and writes, and the VRAM/palette memory behind them.

use super::*;

/// Dots between a 'write' of `$2006` and the PPU loading `v` from it.
const ADDR_WRITE_DELAY: u8 = 3;

impl Ppu {
    /// Drives the PPU's data bus, and notes when.
    ///
    /// The bus is a set of capacitors rather than a register: nothing holds a
    /// value there, it merely takes a while to leak away. See
    /// [`Ppu::decay_bus`].
    fn drive_bus(&mut self, val: u8) {
        self.open_bus = val;
        self.open_bus_frame = self.frame;
    }

    /// Lets the data bus leak away once it has been left alone long enough.
    /// Called once a frame; see [`BUS_DECAY_FRAMES`].
    pub(super) fn decay_bus(&mut self) {
        if self.frame.wrapping_sub(self.open_bus_frame) >= BUS_DECAY_FRAMES {
            self.open_bus = 0;
        }
    }

    /// A read of `$2002` beginning: `M2` has gone high. Bit 7 is the only one
    /// of the three flags with a latch behind it, and this is where that latch
    /// is filled and the flag it copied cleared. That is a dot before the byte
    /// is handed over in [`Ppu::read_register`], which finds the other two
    /// wherever they have got to by then.
    pub fn begin_status_read(&mut self) {
        // A read landing on the dot before the flag would be set wins the
        // race: the flag never appears, and no NMI with it.
        if self.scanline == self.vblank_scanline() && self.dot == 1 {
            self.vbl_suppress = true;
        }
        self.vbl_latch = self.status & 0x80;
        self.status &= !0x80; // reading clears vblank
    }

    pub fn read_register(&mut self, addr: u16, mapper: &mut dyn Mapper, cpu_open_bus: u8) -> u8 {
        let _ = cpu_open_bus;
        match addr & 7 {
            2 => {
                // Bit 7 comes from the latch filled when the access began; the
                // sprite flags are taken live, as it ends. The low five bits
                // are whatever was last on the PPU bus.
                let val = self.vbl_latch | (self.status & 0x60) | (self.open_bus & 0x1f);
                self.w = false;
                self.drive_bus(val);
                val
            }
            4 => {
                // While rendering, this register does not report OAM at all:
                // it reports the byte the sprite unit has in hand, whatever
                // stage of its work that came from.
                //
                // The processor's access lands a dot before the sprite unit's
                // own phase boundaries, so what it finds is the buffer as the
                // dot the access completes on left it.
                let val = if self.mask_rendering() && self.scanline < 240 {
                    self.oam_buffer
                } else {
                    let mut v = self.oam[self.oam_addr as usize];
                    // Attribute bytes have three bits that do not exist in OAM.
                    if self.oam_addr & 3 == 2 {
                        v &= 0xe3;
                    }
                    v
                };
                self.drive_bus(val);
                val
            }
            7 => {
                let addr = self.v & 0x3fff;
                let val = if addr >= 0x3f00 {
                    // Palette reads bypass the buffer, but the buffer is still
                    // filled from the nametable underneath the palette.
                    self.read_buffer = self.read_vram(addr & 0x2fff, mapper);
                    // Palette entries are six bits. The upper two have no
                    // memory behind them and come off the data bus.
                    (self.read_palette(addr) & 0x3f) | (self.open_bus & 0xc0)
                } else if self.mask_rendering() && self.scanline < 240 {
                    // Rendering owns the bus. The access puts its address
                    // out, which is enough to disturb a fetch in flight:
                    // only the low lines are latched. But the byte the
                    // buffer ends up with is the one the renderer reads two
                    // dots from now, not one of this register's own.
                    let buffered = self.read_buffer;
                    self.pending_fetch = Some((addr, ADDR_WRITE_DELAY));
                    self.pending_any = true;
                    buffered
                } else {
                    let buffered = self.read_buffer;
                    self.read_buffer = self.read_vram(addr, mapper);
                    buffered
                };
                self.increment_v(mapper);
                self.drive_bus(val);
                val
            }
            // $2000, $2001, $2003, $2005 and $2006 are write-only.
            _ => self.open_bus,
        }
    }

    /// Take a 'write' off the processor.
    ///
    /// Everything is acted on where it lands except `$2006`, which the PPU
    /// takes its time over: the scroll counter is loaded three dots later, and
    /// a fetch in between reads from an address that is half old and half new.
    /// Page 21's `Hybrid Addresses` is about exactly that gap.
    pub fn write_register(&mut self, addr: u16, val: u8, mapper: &mut dyn Mapper) {
        self.drive_bus(val);
        if addr & 7 == 6 {
            self.pending_write = Some((addr, val, ADDR_WRITE_DELAY));
            self.pending_any = true;
        } else {
            self.apply_register_write(addr, val, mapper);
        }
    }

    /// Act on whatever the PPU has been holding on to.
    pub(super) fn apply_write(&mut self, mapper: &mut dyn Mapper) {
        if let Some((addr, left)) = self.pending_fetch {
            if left > 1 {
                self.pending_fetch = Some((addr, left - 1));
            } else {
                self.pending_fetch = None;
                // The renderer's own ALE on this dot lands after this one and
                // takes the bus, which is what the hardware does too.
                self.ale(addr);
                // Two dots from here the byte arrives; the counter is read
                // one dot before it reaches zero, so three.
                self.buffer_fill = 3;
            }
        }

        if let Some(left) = self.pending_increment {
            if left > 1 {
                self.pending_increment = Some(left - 1);
            } else {
                self.pending_increment = None;
                self.increment_x();
                self.increment_y();
                self.bus_addr = self.v & 0x3fff;
            }
        }

        let Some((addr, val, left)) = self.pending_write else {
            self.pending_any = self.pending_increment.is_some() || self.pending_fetch.is_some();
            return;
        };
        if left > 1 {
            self.pending_write = Some((addr, val, left - 1));
            return;
        }
        self.pending_write = None;
        self.pending_any = self.pending_increment.is_some() || self.pending_fetch.is_some();
        self.apply_register_write(addr, val, mapper);
    }

    fn apply_register_write(&mut self, addr: u16, val: u8, mapper: &mut dyn Mapper) {
        mapper.ppu_register_write(0x2000 | (addr & 7), val);
        self.set_mirroring(mapper.mirroring());
        match addr & 7 {
            0 => {
                self.ctrl = val;
                self.nmi_output = val & 0x80 != 0;
                // Nametable select occupies bits 10-11 of the scroll latch.
                self.t = (self.t & 0xf3ff) | (((val & 0x03) as u16) << 10);
            }
            1 => {
                self.mask = val;
                self.refresh_emitted();
                if self.rendering_enabled != self.mask_rendering() {
                    self.need_state_update = true;
                }
            }
            3 => self.oam_addr = val,
            4 => {
                // While rendering, the sprite unit owns OAM and the write
                // never reaches it. All it does is push the address on by one
                // object, aligned down — the address is following the unit's
                // own stepping rather than the processor's byte at a time.
                let rendering_line = self.scanline < 240;
                if self.mask_rendering() && rendering_line {
                    self.oam_addr = self.oam_addr.wrapping_add(4) & 0xfc;
                } else {
                    self.oam[self.oam_addr as usize] = val;
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                }
            }
            5 => {
                if !self.w {
                    self.t = (self.t & 0xffe0) | (val as u16 >> 3);
                    self.x = val & 0x07;
                } else {
                    self.t = (self.t & 0x8c1f)
                        | (((val & 0x07) as u16) << 12)
                        | (((val & 0xf8) as u16) << 2);
                }
                self.w = !self.w;
            }
            6 => {
                if !self.w {
                    // Bit 14 is cleared: the address space is only 14 bits.
                    self.t = (self.t & 0x00ff) | (((val & 0x3f) as u16) << 8);
                } else {
                    self.t = (self.t & 0xff00) | val as u16;
                    self.v = self.t;
                    self.bus_addr = self.v & 0x3fff;
                }
                self.w = !self.w;
            }
            7 => {
                let addr = self.v & 0x3fff;
                if addr >= 0x3f00 {
                    self.write_palette(addr, val);
                } else {
                    self.write_vram(addr, val, mapper);
                }
                self.increment_v(mapper);
            }
            _ => {}
        }
    }

    fn increment_v(&mut self, mapper: &mut dyn Mapper) {
        // During rendering, $2007 access instead steps the scroll counters,
        // which is why writing to it mid-frame corrupts the display. The step
        // is taken as late as a `$2006` 'writes' is, so the fetch the access
        // lands on still runs off the address it had.
        if self.mask_rendering() && self.scanline < 240 {
            self.pending_increment = Some(ADDR_WRITE_DELAY);
            self.pending_any = true;
            return;
        } else {
            let step = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
            self.v = (self.v + step) & 0x7fff;
        }
        self.bus_addr = self.v & 0x3fff;
        let _ = mapper;
    }

    // ---- PPU bus --------------------------------------------------------

    /// Work out which of the console's two nametables each of the four
    /// quadrants is wired to, and remember it.
    ///
    /// The cartridge decides this and only ever changes its mind when it is
    /// written to, so the answer is taken there rather than asked of it at
    /// every read of a nametable.
    pub fn set_mirroring(&mut self, mirroring: Mirroring) {
        for table in 0..4 {
            let bank = match mirroring {
                Mirroring::Horizontal => table >> 1,
                Mirroring::Vertical => table & 1,
                Mirroring::SingleScreenA => 0,
                Mirroring::SingleScreenB => 1,
                // Without cartridge VRAM, four-screen falls back to the
                // console's two banks; real four-screen mappers intercept
                // before this.
                Mirroring::FourScreen => table & 1,
                Mirroring::Quadrants(which) => which[table] as usize,
            };
            self.nt_bank[table] = bank * 0x400;
        }
    }

    fn nametable_index(&self, addr: u16) -> usize {
        let addr = (addr - 0x2000) & 0x0fff;
        self.nt_bank[(addr >> 10) as usize] + (addr & 0x3ff) as usize
    }

    /// Put an address on the bus and latch its low byte: the first dot of access. 
    /// See [`Ppu::ale_latch`] for why the two halves part company.
    pub(super) fn ale(&mut self, addr: u16) {
        let addr = addr & 0x3fff;
        // The low eight address lines are the data lines. An ALE that falls
        // on the dot a read completes on therefore latches whatever the data
        // lines are carrying, not the address being driven -- the latch and
        // the bus feed each other, and the bus wins.
        self.ale_latch = if self.buffer_fill == 1 { self.bus_data } else { addr as u8 };
        self.bus_addr = addr;
    }

    /// Read on the second dot of access.
    ///
    /// `driven` is the address the PPU is putting out *now* — only its upper
    /// lines are used, because the lower ones come from whatever was latched
    /// on the first dot. Give it the same address the access started with and
    /// nothing has moved; give it a different one and the read comes from the
    /// two halves mixed, which is what page 21 is about.
    pub(super) fn read_latched(&mut self, driven: u16, mapper: &mut dyn Mapper) -> u8 {
        let addr = (driven & 0x3f00) | self.ale_latch as u16;
        self.read_vram(addr, mapper)
    }

    pub(super) fn read_vram(&mut self, addr: u16, mapper: &mut dyn Mapper) -> u8 {
        let val = self.read_vram_raw(addr, mapper);
        self.bus_data = val;
        val
    }

    fn read_vram_raw(&mut self, addr: u16, mapper: &mut dyn Mapper) -> u8 {
        let addr = addr & 0x3fff;
        self.bus_addr = addr;
        if self.mapper_watches_bus {
            mapper.ppu_bus_read(addr);
        }
        match addr {
            0x0000..=0x1fff => mapper.ppu_read(addr),
            0x2000..=0x3eff => {
                let addr = 0x2000 | (addr & 0x0fff);
                if self.mapper_serves_nametables {
                    if let Some(val) = mapper.read_nametable(addr) {
                        return val;
                    }
                }
                self.vram[self.nametable_index(addr)]
            }
            _ => self.read_palette(addr),
        }
    }

    fn write_vram(&mut self, addr: u16, val: u8, mapper: &mut dyn Mapper) {
        let addr = addr & 0x3fff;
        self.bus_addr = addr;
        match addr {
            0x0000..=0x1fff => mapper.ppu_write(addr, val),
            0x2000..=0x3eff => {
                let addr = 0x2000 | (addr & 0x0fff);
                if self.mapper_serves_nametables && mapper.write_nametable(addr, val) {
                    return;
                }
                let idx = self.nametable_index(addr);
                self.vram[idx] = val;
            }
            _ => self.write_palette(addr, val),
        }
    }

    /// Entries $3F10/$14/$18/$1C mirror the backdrop color at $3F00.
    #[rustfmt::skip]
    fn palette_index(addr: u16) -> usize {
        let idx = (addr & 0x1f) as usize;
        if idx & 0x13 == 0x10 { idx & 0x0f } else { idx }
    }

    #[rustfmt::skip]
    pub(super) fn read_palette(&self, addr: u16) -> u8 {
        let val = self.palette[Self::palette_index(addr)];
        // Greyscale is applied on the way out of the palette, not at mixing.
        if self.mask & 0x01 != 0 { val & 0x30 } else { val }
    }

    fn write_palette(&mut self, addr: u16, val: u8) {
        self.palette[Self::palette_index(addr)] = val & 0x3f;
        self.refresh_emitted();
    }

    /// Work out what every palette entry now emits. See the field.
    pub(super) fn refresh_emitted(&mut self) {
        let grey = self.mask & 0x01 != 0;
        let emphasis = (self.mask as u16 >> 5) << 6;
        for i in 0..32 {
            let mut val = self.palette[Self::palette_index(0x3f00 + i as u16)];
            if grey {
                val &= 0x30;
            }
            self.emitted[i] = (val as u16 & 0x3f) | emphasis;
        }
    }
}
