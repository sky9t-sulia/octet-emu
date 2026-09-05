//! CPU-facing register reads and writes, and the VRAM/palette memory behind them.

use super::*;

impl Ppu {
    pub fn read_register(&mut self, addr: u16, mapper: &mut dyn Mapper, cpu_open_bus: u8) -> u8 {
        let _ = cpu_open_bus;
        match addr & 7 {
            2 => {
                // A read landing on the dot before the flag would be set wins
                // the race: the flag never appears.
                if self.scanline == self.vblank_scanline() && self.dot == 1 {
                    self.vbl_suppress = true;
                }
                // The low five bits are whatever was last on the PPU bus.
                let val = (self.status & 0xe0) | (self.open_bus & 0x1f);
                self.status &= !0x80; // reading clears vblank
                self.w = false;
                self.open_bus = val;
                val
            }
            4 => {
                let mut val = self.oam[self.oam_addr as usize];
                // Attribute bytes have three bits that do not exist in OAM.
                if self.oam_addr & 3 == 2 {
                    val &= 0xe3;
                }
                self.open_bus = val;
                val
            }
            7 => {
                let addr = self.v & 0x3fff;
                let val = if addr >= 0x3f00 {
                    // Palette reads bypass the buffer, but the buffer is still
                    // filled from the nametable underneath the palette.
                    self.read_buffer = self.read_vram(addr & 0x2fff, mapper);
                    self.read_palette(addr)
                } else {
                    let buffered = self.read_buffer;
                    self.read_buffer = self.read_vram(addr, mapper);
                    buffered
                };
                self.increment_v(mapper);
                self.open_bus = val;
                val
            }
            // $2000, $2001, $2003, $2005 and $2006 are write-only.
            _ => self.open_bus,
        }
    }

    pub fn write_register(&mut self, addr: u16, val: u8, mapper: &mut dyn Mapper) {
        self.open_bus = val;
        mapper.ppu_register_write(0x2000 | (addr & 7), val);
        match addr & 7 {
            0 => {
                self.ctrl = val;
                self.nmi_output = val & 0x80 != 0;
                // Nametable select occupies bits 10-11 of the scroll latch.
                self.t = (self.t & 0xf3ff) | (((val & 0x03) as u16) << 10);
            }
            1 => {
                self.mask = val;
                if self.rendering_enabled != self.mask_rendering() {
                    self.need_state_update = true;
                }
            }
            3 => self.oam_addr = val,
            4 => {
                self.oam[self.oam_addr as usize] = val;
                self.oam_addr = self.oam_addr.wrapping_add(1);
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
        // which is why writing to it mid-frame corrupts the display.
        if self.mask_rendering() && self.scanline < 240 {
            self.increment_x();
            self.increment_y();
        } else {
            let step = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
            self.v = (self.v + step) & 0x7fff;
        }
        self.bus_addr = self.v & 0x3fff;
        let _ = mapper;
    }

    // ---- PPU bus --------------------------------------------------------

    fn nametable_index(&self, addr: u16, mirroring: Mirroring) -> usize {
        let addr = (addr - 0x2000) & 0x0fff;
        let table = (addr / 0x400) as usize;
        let offset = (addr % 0x400) as usize;
        let bank = match mirroring {
            Mirroring::Horizontal => table >> 1,
            Mirroring::Vertical => table & 1,
            Mirroring::SingleScreenA => 0,
            Mirroring::SingleScreenB => 1,
            // Without cartridge VRAM, four-screen falls back to the console's
            // two banks; real four-screen mappers intercept before this.
            Mirroring::FourScreen => table & 1,
        };
        bank * 0x400 + offset
    }

    pub(super) fn read_vram(&mut self, addr: u16, mapper: &mut dyn Mapper) -> u8 {
        let addr = addr & 0x3fff;
        self.bus_addr = addr;
        mapper.ppu_bus_read(addr);
        match addr {
            0x0000..=0x1fff => mapper.ppu_read(addr),
            0x2000..=0x3eff => {
                let addr = 0x2000 | (addr & 0x0fff);
                if let Some(val) = mapper.read_nametable(addr) {
                    return val;
                }
                self.vram[self.nametable_index(addr, mapper.mirroring())]
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
                if mapper.write_nametable(addr, val) {
                    return;
                }
                let idx = self.nametable_index(addr, mapper.mirroring());
                self.vram[idx] = val;
            }
            _ => self.write_palette(addr, val),
        }
    }

    /// Entries $3F10/$14/$18/$1C mirror the backdrop colour at $3F00.
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
    }
}
