//! Mapper 11 — Color Dreams. One register drives both a 32 KiB PRG bank in
//! the low bits and an 8 KiB CHR bank in the high bits.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct ColorDreams {
    banks: Banks,
    mirroring: Mirroring,
}

impl ColorDreams {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut banks = Banks::new(cart);
        banks.map_prg_32k(0);
        banks.map_chr_8k(0);
        ColorDreams { banks, mirroring }
    }
}

impl Mapper for ColorDreams {
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
        self.banks.map_prg_32k((val & 0x03) as isize);
        self.banks.map_chr_8k((val >> 4) as isize);
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
}
