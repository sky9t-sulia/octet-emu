//! Mapper 28 — Action 53.
//!
//! A homebrew multicart board. Its PRG space is split into an outer bank
//! selecting one game within the ROM and an inner bank selecting a page
//! within that game, with the game's size deciding how many bits each
//! contributes. Mirroring is unusual too: single-screen games let the board
//! pick the page from a bit of whichever register was last written.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Action53 {
    banks: Banks,
    selected: usize,
    regs: [u8; 4],
    /// Single-screen page, written from a spare bit of registers 0-2.
    mirroring_bit: u8,
    mirroring: Mirroring,
}

impl Action53 {
    pub fn new(cart: Cartridge) -> Self {
        let mut banks = Banks::new(cart);
        banks.map_prg_16k(1, -1);
        banks.map_chr_8k(0);
        let mut m = Action53 {
            banks,
            selected: 0,
            regs: [0; 4],
            mirroring_bit: 0,
            mirroring: Mirroring::SingleScreenA,
        };
        m.sync();
        m
    }

    fn sync(&mut self) {
        // Bit 1 of the mirroring field chooses between the two-screen modes;
        // with it clear the board falls back to the latched single-screen bit.
        let mut mirroring = self.regs[2] & 0x03;
        if mirroring & 0x02 == 0 {
            mirroring = self.mirroring_bit;
        }
        self.mirroring = match mirroring {
            0 => Mirroring::SingleScreenA,
            1 => Mirroring::SingleScreenB,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal,
        };

        let game_size = ((self.regs[2] & 0x30) >> 4) as usize;
        let prg_16k = self.regs[2] & 0x08 != 0;
        let slot_select = ((self.regs[2] & 0x04) >> 2) as isize;
        let chr_select = (self.regs[0] & 0x03) as isize;
        let prg_select = (self.regs[1] & 0x0f) as isize;
        let outer = (self.regs[3] as isize) << 1;

        self.banks.map_chr_8k(chr_select);

        // How much of the bank number the game itself supplies, by game size.
        const OUTER_MASK: [isize; 4] = [0x1fe, 0x1fc, 0x1f8, 0x1f0];
        const INNER_MASK: [isize; 4] = [0x01, 0x03, 0x07, 0x0f];

        if prg_16k {
            // One switchable 16 KiB window; the other holds the game's fixed
            // page, which is whichever half `slot_select` did not choose.
            let bank = if slot_select != 0 { 0 } else { 1 } as usize;
            let selected = (outer & OUTER_MASK[game_size]) | (prg_select & INNER_MASK[game_size]);
            self.banks.map_prg_16k(bank, selected);
            self.banks.map_prg_16k(slot_select as usize, (outer & 0x1fe) | slot_select);
        } else {
            // 32 KiB mode: the pair of windows moves together.
            let prg_select = prg_select << 1;
            self.banks.map_prg_16k(
                0,
                (outer & OUTER_MASK[game_size]) | (prg_select & INNER_MASK[game_size]),
            );
            self.banks.map_prg_16k(
                1,
                (outer & OUTER_MASK[game_size]) | ((prg_select | 0x01) & INNER_MASK[game_size]),
            );
        }
    }
}

impl Mapper for Action53 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            // The register index lives in two non-adjacent bits of the value.
            0x5000..=0x5fff => self.selected = (((val & 0x80) >> 6) | (val & 0x01)) as usize,
            0x8000..=0xffff => {
                if self.selected <= 1 {
                    self.mirroring_bit = (val >> 4) & 0x01;
                } else if self.selected == 2 {
                    self.mirroring_bit = val & 0x01;
                }
                self.regs[self.selected] = val;
                self.sync();
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
        self.mirroring
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
