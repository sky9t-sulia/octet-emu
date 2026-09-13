//! Mapper 75 — Konami's VRC1, the plainest of the family. Three switchable
//! 8 KiB PRG windows with the last fixed, two 4 KiB CHR windows, and one
//! register that carries mirroring together with the fifth bit of each CHR
//! bank — the chip had no pins to spare for them anywhere else.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Vrc1 {
    banks: Banks,
    chr_low: [u8; 2],
    chr_high: [u8; 2],
    mirroring: Mirroring,
}

impl Vrc1 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut board =
            Vrc1 { banks: Banks::new(cart), chr_low: [0; 2], chr_high: [0; 2], mirroring };
        board.banks.map_prg_8k(0, 0);
        board.banks.map_prg_8k(1, 1);
        board.banks.map_prg_8k(2, -2);
        board.banks.map_prg_8k(3, -1);
        board.sync_chr();
        board
    }

    fn sync_chr(&mut self) {
        for half in 0..2 {
            let bank = ((self.chr_high[half] as isize) << 4) | self.chr_low[half] as isize;
            self.banks.map_chr_4k(half, bank);
        }
    }
}

impl Mapper for Vrc1 {
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
            0x8000 => self.banks.map_prg_8k(0, (val & 0x0f) as isize),
            0xa000 => self.banks.map_prg_8k(1, (val & 0x0f) as isize),
            0xc000 => self.banks.map_prg_8k(2, (val & 0x0f) as isize),
            0x9000 => {
                self.mirroring =
                    if val & 1 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical };
                self.chr_high[0] = (val >> 1) & 1;
                self.chr_high[1] = (val >> 2) & 1;
                self.sync_chr();
            }
            0xe000 => {
                self.chr_low[0] = val & 0x0f;
                self.sync_chr();
            }
            0xf000 => {
                self.chr_low[1] = val & 0x0f;
                self.sync_chr();
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

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
