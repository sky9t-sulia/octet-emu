//! Mapper 232 — Camerica BF9096, the chip behind the Quattro compilations.
//!
//! A multicart in one chip: the four games sit in 64 KiB blocks, and the
//! register at `$8000` picks the block while the one at `$C000` picks a 16 KiB
//! page inside it. The upper window is fixed to the block's last page rather
//! than the cartridge's last, so each game sees the map its own build expected
//! and none of them knows the others are there.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Bf9096 {
    banks: Banks,
    block: u8,
    page: u8,
    /// The Aladdin Deck Enhancer wires the two block lines the other way
    /// round. NES 2.0 submapper 1.
    swapped: bool,
    mirroring: Mirroring,
}

impl Bf9096 {
    pub fn new(cart: Cartridge, submapper: u8) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Bf9096 {
            banks: Banks::new(cart),
            block: 0,
            page: 0,
            swapped: submapper == 1,
            mirroring,
        };
        board.sync();
        board
    }

    fn sync(&mut self) {
        let block = if self.swapped { ((self.block << 1) | (self.block >> 1)) & 3 } else { self.block };
        let base = (block as isize) * 4;
        self.banks.map_prg_16k(0, base + self.page as isize);
        self.banks.map_prg_16k(1, base + 3);
        self.banks.map_chr_8k(0);
    }
}

impl Mapper for Bf9096 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            ..=0x7fff => {
                self.banks.write_cpu(addr, val);
            }
            0x8000..=0xbfff => {
                self.block = (val >> 3) & 3;
                self.sync();
            }
            _ => {
                self.page = val & 3;
                self.sync();
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
