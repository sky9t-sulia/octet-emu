//! Mapper 0 — NROM. No banking at all: 16 or 32 KiB of PRG, 8 KiB of CHR.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Nrom {
    banks: Banks,
    mirroring: Mirroring,
}

impl Nrom {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut banks = Banks::new(cart);
        // A 16 KiB image appears twice; `map_prg_16k` wraps to do that for us.
        banks.map_prg_16k(0, 0);
        banks.map_prg_16k(1, 1);
        banks.map_chr_8k(0);
        Nrom { banks, mirroring }
    }
}

impl Mapper for Nrom {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        self.banks.write_cpu(addr, val);
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
