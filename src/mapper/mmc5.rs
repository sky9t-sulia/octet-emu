//! Mapper 5 — MMC5, the most capable chip Nintendo shipped.
//!
//! It is less a mapper than a coprocessor: four PRG windows that can each be
//! ROM or RAM, two independent CHR bank sets swapped automatically between
//! background and sprite fetches, a kilobyte of expansion RAM usable as a
//! nametable, programmable fill-mode tiles, an 8x8 multiplier and a scanline
//! counter.
//!
//! The scanline counter is the subtle part. The MMC5 has no connection to the
//! PPU's timing signals, so it infers where the PPU is from the shape of its
//! reads: three consecutive reads of the same nametable address only happen at
//! the end of a scanline, and that is what it counts.
//!
//! Not yet implemented: the vertical split screen, extended attribute mode,
//! and the MMC5's own sound channels.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Mmc5 {
    banks: Banks,

    prg_mode: u8,
    prg_banks: [u8; 5],
    prg_ram_protect1: u8,
    prg_ram_protect2: u8,

    chr_mode: u8,
    chr_upper_bits: u16,
    chr_banks: [u16; 12],
    /// Which CHR register was written last. Together with the sprite size it
    /// decides whether background fetches use bank set A or B.
    last_chr_reg: u16,
    prev_chr_a: bool,
    large_sprites: bool,
    rendering_enabled: bool,

    /// The MMC5 supplies all four nametables itself, so it owns the console's
    /// CIRAM as well as its own expansion RAM.
    #[cfg_attr(feature = "snapshot", serde(with = "crate::snapshot::bytes"))]
    ciram: [u8; 0x800],
    exram: Vec<u8>,
    extended_ram_mode: u8,
    nametable_mapping: u8,
    fill_tile: u8,
    fill_attr: u8,

    multiplicand: u8,
    multiplier: u8,

    // ---- Scanline counter ----
    irq_target: u8,
    irq_enabled: bool,
    irq_pending: bool,
    scanline: u8,
    ppu_in_frame: bool,
    need_in_frame: bool,
    nt_read_counter: u8,
    last_ppu_read_addr: u16,
    /// Counts down from three CPU cycles since the last PPU read; when it
    /// expires the PPU must have stopped rendering.
    ppu_idle_counter: u8,
    tile_number: i32,
}

impl Mmc5 {
    pub fn new(cart: Cartridge) -> Self {
        let mut mmc5 = Mmc5 {
            banks: Banks::new(cart),
            prg_mode: 3,
            prg_banks: [0, 0, 0, 0, 0xff],
            prg_ram_protect1: 0,
            prg_ram_protect2: 0,
            chr_mode: 0,
            chr_upper_bits: 0,
            chr_banks: [0; 12],
            last_chr_reg: 0,
            prev_chr_a: true,
            large_sprites: false,
            rendering_enabled: false,
            ciram: [0; 0x800],
            exram: vec![0; 0x400],
            extended_ram_mode: 0,
            nametable_mapping: 0,
            fill_tile: 0,
            fill_attr: 0,
            multiplicand: 0,
            multiplier: 0,
            irq_target: 0,
            irq_enabled: false,
            irq_pending: false,
            scanline: 0,
            ppu_in_frame: false,
            need_in_frame: false,
            nt_read_counter: 0,
            last_ppu_read_addr: 0,
            ppu_idle_counter: 0,
            tile_number: 0,
        };
        // Games expect the last PRG page to be swapped in at power-on, and
        // 8 KiB PRG mode to be selected.
        mmc5.update_prg();
        mmc5.update_chr(true);
        mmc5
    }

    // ---- PRG ------------------------------------------------------------

    /// Decode a PRG bank register: bit 7 selects ROM, clear selects RAM.
    /// `$5117` is always ROM and `$5113` always RAM.
    fn is_ram_bank(&self, reg: usize) -> bool {
        let value = self.prg_banks[reg];
        reg == 0 || (value & 0x80 == 0 && reg != 4)
    }

    fn ram_writable(&self) -> bool {
        // Both protection registers must hold their unlock values.
        self.prg_ram_protect1 == 0x02 && self.prg_ram_protect2 == 0x01
    }

    fn map_window(&mut self, slot: usize, reg: usize, bank: isize) {
        if self.is_ram_bank(reg) {
            self.banks.map_prg_8k_ram(slot, bank & 0x07);
        } else {
            self.banks.map_prg_8k(slot, bank & 0x7f);
        }
    }

    fn update_prg(&mut self) {
        // $6000 always comes from RAM, selected by $5113.
        let wram = (self.prg_banks[0] & 0x07) as isize;
        self.banks.map_wram(Some(wram), !self.ram_writable());

        match self.prg_mode {
            // One 32 KiB ROM bank across the whole space.
            0 => {
                let bank = (self.prg_banks[4] & 0x7c) as isize;
                for i in 0..4 {
                    self.banks.map_prg_8k(i, bank + i as isize);
                }
            }
            // 16 KiB at $8000 (which may be RAM) plus 16 KiB of ROM at $C000.
            1 => {
                let lo = (self.prg_banks[2] & 0xfe) as isize;
                self.map_window(0, 2, lo);
                self.map_window(1, 2, lo + 1);
                let hi = (self.prg_banks[4] & 0x7e) as isize;
                self.banks.map_prg_8k(2, hi);
                self.banks.map_prg_8k(3, hi + 1);
            }
            // 16 KiB, then two 8 KiB windows.
            2 => {
                let lo = (self.prg_banks[2] & 0xfe) as isize;
                self.map_window(0, 2, lo);
                self.map_window(1, 2, lo + 1);
                let c = self.prg_banks[3] as isize;
                self.map_window(2, 3, c);
                self.banks.map_prg_8k(3, (self.prg_banks[4] & 0x7f) as isize);
            }
            // Four independent 8 KiB windows.
            _ => {
                let a = self.prg_banks[1] as isize;
                self.map_window(0, 1, a);
                let b = self.prg_banks[2] as isize;
                self.map_window(1, 2, b);
                let c = self.prg_banks[3] as isize;
                self.map_window(2, 3, c);
                self.banks.map_prg_8k(3, (self.prg_banks[4] & 0x7f) as isize);
            }
        }
    }

    // ---- CHR ------------------------------------------------------------

    /// With 8x16 sprites, `$5120-$5127` serve sprite fetches and `$5128-$512B`
    /// background ones, told apart by where the PPU is in the scanline.
    fn update_chr(&mut self, force: bool) {
        if !self.large_sprites {
            // 8x8 sprites use one bank set, and reset the last-written logic.
            self.last_chr_reg = 0;
        }
        let chr_a = !self.large_sprites
            || (self.tile_number >= 32 && self.tile_number < 48)
            || (!self.ppu_in_frame && self.last_chr_reg <= 0x5127);
        if !force && chr_a == self.prev_chr_a {
            return;
        }
        self.prev_chr_a = chr_a;

        let b = |lo: usize, hi: usize| self.chr_banks[if chr_a { lo } else { hi }] as isize;
        match self.chr_mode {
            0 => self.banks.map_chr_8k(b(0x07, 0x0b)),
            1 => {
                self.banks.map_chr_4k(0, b(0x03, 0x0b));
                self.banks.map_chr_4k(1, b(0x07, 0x0b));
            }
            2 => {
                self.banks.map_chr_2k(0, b(0x01, 0x09));
                self.banks.map_chr_2k(1, b(0x03, 0x0b));
                self.banks.map_chr_2k(2, b(0x05, 0x09));
                self.banks.map_chr_2k(3, b(0x07, 0x0b));
            }
            _ => {
                for i in 0..8 {
                    let lo = i;
                    let hi = 0x08 + (i & 3);
                    self.banks.map_chr_1k(i, b(lo, hi));
                }
            }
        }
    }

    // ---- Nametables -----------------------------------------------------

    /// Source feeding one of the four nametable slots, from `$5105`.
    fn nametable_source(&self, slot: usize) -> u8 {
        (self.nametable_mapping >> (slot * 2)) & 0x03
    }

    // ---- Scanline detection ---------------------------------------------

    fn detect_scanline_start(&mut self, addr: u16) {
        if self.nt_read_counter >= 2 {
            // Three identical nametable reads mean the PPU has reached the end
            // of a line; the next read starts the new one.
            if !self.ppu_in_frame && !self.need_in_frame {
                self.need_in_frame = true;
                self.scanline = 0;
            } else {
                self.scanline = self.scanline.wrapping_add(1);
                if self.scanline == self.irq_target {
                    self.irq_pending = true;
                }
            }
        } else if (0x2000..0x3000).contains(&addr) && self.last_ppu_read_addr == addr {
            self.nt_read_counter += 1;
            if self.nt_read_counter >= 2 {
                self.tile_number = 0;
            }
        }

        if self.last_ppu_read_addr != addr {
            self.nt_read_counter = 0;
        }
    }
}

impl Mapper for Mmc5 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        match addr {
            // Expansion RAM is only readable in modes 2 and 3.
            0x5c00..=0x5fff => {
                if self.extended_ram_mode >= 2 {
                    Some(self.exram[addr as usize - 0x5c00])
                } else {
                    None
                }
            }
            0x5204 => {
                // Reading the status acknowledges the interrupt, so the
                // side-effect-free peek cannot do it here.
                let mut val = 0;
                if self.ppu_in_frame {
                    val |= 0x40;
                }
                if self.irq_pending {
                    val |= 0x80;
                }
                Some(val)
            }
            0x5205 => Some((self.multiplicand as u16 * self.multiplier as u16) as u8),
            0x5206 => Some(((self.multiplicand as u16 * self.multiplier as u16) >> 8) as u8),
            _ => self.banks.read_cpu(addr),
        }
    }

    fn cpu_read(&mut self, addr: u16) -> Option<u8> {
        let val = self.cpu_peek(addr);
        if addr == 0x5204 {
            self.irq_pending = false;
        }
        val
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x5100 => {
                self.prg_mode = val & 0x03;
                self.update_prg();
            }
            0x5101 => {
                self.chr_mode = val & 0x03;
                self.update_chr(true);
            }
            0x5102 => {
                self.prg_ram_protect1 = val & 0x03;
                self.update_prg();
            }
            0x5103 => {
                self.prg_ram_protect2 = val & 0x03;
                self.update_prg();
            }
            0x5104 => self.extended_ram_mode = val & 0x03,
            0x5105 => self.nametable_mapping = val,
            0x5106 => self.fill_tile = val,
            0x5107 => {
                // The fill-mode colour is repeated across all four corners of
                // the attribute byte.
                let c = val & 0x03;
                self.fill_attr = c | (c << 2) | (c << 4) | (c << 6);
            }
            0x5113..=0x5117 => {
                self.prg_banks[addr as usize - 0x5113] = val;
                self.update_prg();
            }
            0x5120..=0x512b => {
                let reg = addr as usize - 0x5120;
                self.chr_banks[reg] = val as u16 | (self.chr_upper_bits << 8);
                self.last_chr_reg = addr;
                self.update_chr(true);
            }
            0x5130 => self.chr_upper_bits = (val & 0x03) as u16,
            0x5203 => self.irq_target = val,
            0x5204 => {
                self.irq_enabled = val & 0x80 != 0;
            }
            0x5205 => self.multiplicand = val,
            0x5206 => self.multiplier = val,
            0x5c00..=0x5fff => {
                // In modes 0 and 1 expansion RAM only accepts writes while the
                // PPU is rendering; otherwise a zero is stored instead.
                let val = if self.extended_ram_mode <= 1 && !self.ppu_in_frame { 0 } else { val };
                if self.extended_ram_mode != 3 {
                    self.exram[addr as usize - 0x5c00] = val;
                }
            }
            _ => {
                self.banks.write_cpu(addr, val);
            }
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.banks.read_ppu(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.banks.write_ppu(addr, val);
    }

    fn mirroring(&self) -> Mirroring {
        // Unused: the MMC5 intercepts nametable access entirely.
        Mirroring::Horizontal
    }

    #[rustfmt::skip]
    fn watches_ppu_bus(&self) -> bool {
        true
    }

    fn serves_nametables(&self) -> bool {
        true
    }

    fn read_nametable(&mut self, addr: u16) -> Option<u8> {
        let slot = ((addr >> 10) & 3) as usize;
        let offset = (addr & 0x3ff) as usize;
        Some(match self.nametable_source(slot) {
            0 => self.ciram[offset],
            1 => self.ciram[0x400 + offset],
            2 => {
                if self.extended_ram_mode <= 1 { self.exram[offset] } else { 0 }
            }
            // Fill mode: one tile everywhere, with a matching attribute table.
            _ => {
                if offset < 0x3c0 { self.fill_tile } else { self.fill_attr }
            }
        })
    }

    fn write_nametable(&mut self, addr: u16, val: u8) -> bool {
        let slot = ((addr >> 10) & 3) as usize;
        let offset = (addr & 0x3ff) as usize;
        match self.nametable_source(slot) {
            0 => self.ciram[offset] = val,
            1 => self.ciram[0x400 + offset] = val,
            // Modes 2 and 3 make ExRAM read-only to the PPU.
            2 if self.extended_ram_mode <= 1 => self.exram[offset] = val,
            _ => {} // fill mode is read-only
        }
        true
    }

    fn ppu_bus_read(&mut self, addr: u16) {
        let is_nt_fetch = (0x2000..0x3000).contains(&addr) && (addr & 0x3ff) < 0x3c0;
        if is_nt_fetch {
            self.tile_number += 1;
            if self.ppu_in_frame {
                self.update_chr(false);
            } else if self.need_in_frame {
                self.need_in_frame = false;
                self.ppu_in_frame = true;
                self.update_chr(false);
            }
        }
        self.detect_scanline_start(addr);
        self.ppu_idle_counter = 3;
        self.last_ppu_read_addr = addr;
    }

    fn ppu_register_write(&mut self, addr: u16, val: u8) {
        if addr == 0x2000 {
            self.large_sprites = val & 0x20 != 0;
            self.update_chr(true);
        } else if addr == 0x2001 {
            self.rendering_enabled = val & 0x18 != 0;
        }
    }

    fn ticks_cpu(&self) -> bool {
        true
    }

    fn tick_cpu(&mut self) {
        // Three CPU cycles without a PPU read means rendering has stopped,
        // which is how the chip knows the frame ended.
        if self.ppu_idle_counter > 0 {
            self.ppu_idle_counter -= 1;
            if self.ppu_idle_counter == 0 {
                self.ppu_in_frame = false;
                self.update_chr(true);
            }
        }
    }

    fn irq(&self) -> bool {
        self.irq_pending && self.irq_enabled
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
