//! Mapper 68 — Sunsoft-4. Four 2 KiB CHR banks and a 16 KiB PRG window, plus
//! the thing it is remembered for: the nametables can come out of the pattern
//! ROM instead of the console's own memory, which is how After Burner draws a
//! horizon it could not have drawn in 2 KiB of RAM.
//!
//! Two banks serve the four quadrants, laid out by the mirroring mode as
//! usual. Their registers keep only seven bits and the eighth is taken as set,
//! so the nametables always live in the last 128 KiB of the pattern ROM.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Sunsoft4 {
    banks: Banks,
    /// The two 1 KiB pattern-ROM banks that stand in for nametables.
    nt_banks: [u8; 2],
    mode: u8,
    from_chr: bool,
    mirroring: Mirroring,
}

impl Sunsoft4 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Sunsoft4 {
            banks: Banks::new(cart),
            nt_banks: [0; 2],
            mode: 0,
            from_chr: false,
            mirroring,
        };
        board.banks.map_prg_16k(0, 0);
        board.banks.map_prg_16k(1, -1);
        board.banks.map_chr_8k(0);
        board
    }

    /// Which of the two banks a quadrant is drawn from, by mirroring mode.
    fn quadrant_high(&self, quadrant: usize) -> bool {
        match self.mode {
            0 => quadrant & 1 == 1,
            1 => quadrant >= 2,
            2 => false,
            _ => true,
        }
    }
}

impl Mapper for Sunsoft4 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if addr < 0x8000 {
            self.banks.write_cpu(addr, val);
            return;
        }
        match addr & 0xf000 {
            0x8000 => self.banks.map_chr_2k(0, val as isize),
            0x9000 => self.banks.map_chr_2k(1, val as isize),
            0xa000 => self.banks.map_chr_2k(2, val as isize),
            0xb000 => self.banks.map_chr_2k(3, val as isize),
            0xc000 => self.nt_banks[0] = val,
            0xd000 => self.nt_banks[1] = val,
            0xe000 => {
                self.mode = val & 3;
                self.from_chr = val & 0x10 != 0;
                self.mirroring = match self.mode {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenA,
                    _ => Mirroring::SingleScreenB,
                };
            }
            0xf000 => {
                self.banks.map_prg_16k(0, (val & 0x0f) as isize);
                self.banks.map_wram(if val & 0x10 != 0 { Some(0) } else { None }, false);
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.banks.read_ppu(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.banks.write_ppu(addr, val);
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn serves_nametables(&self) -> bool {
        true
    }

    fn read_nametable(&mut self, addr: u16) -> Option<u8> {
        if !self.from_chr {
            return None;
        }
        let quadrant = ((addr >> 10) & 3) as usize;
        let bank = (self.nt_banks[self.quadrant_high(quadrant) as usize] | 0x80) as usize;
        let len = self.banks.chr_rom.len();
        if len == 0 {
            return Some(0);
        }
        let off = (bank * 1024) % len + (addr as usize & 0x3ff);
        Some(self.banks.chr_rom.get(off).copied().unwrap_or(0))
    }

    fn write_nametable(&mut self, _addr: u16, _val: u8) -> bool {
        // With the pattern ROM in their place there is nothing to write to.
        self.from_chr
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
