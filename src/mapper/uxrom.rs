//! Mapper 2 — UxROM. A switchable 16 KiB bank at $8000, the last bank fixed
//! at $C000, and CHR RAM. No CHR banking at all.

use super::{bus_conflict, Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct UxRom {
    banks: Banks,
    mirroring: Mirroring,
}

impl UxRom {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut banks = Banks::new(cart);
        banks.map_prg_16k(0, 0);
        banks.map_prg_16k(1, -1);
        banks.map_chr_8k(0);
        UxRom { banks, mirroring }
    }
}

impl Mapper for UxRom {
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
        let val = bus_conflict(&self.banks, addr, val);
        self.banks.map_prg_16k(0, (val & 0x0f) as isize);
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
