//! Mapper 7 — AxROM. A 32 KiB PRG bank plus single-screen mirroring chosen by
//! the same register write, which is how these games scroll a single plane.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct AxRom {
    banks: Banks,
    mirroring: Mirroring,
}

impl AxRom {
    pub fn new(cart: Cartridge) -> Self {
        let mut banks = Banks::new(cart);
        banks.map_prg_32k(0);
        banks.map_chr_8k(0);
        // The header's mirroring bit is meaningless here: the board is always
        // single-screen, selected by the mapper register.
        AxRom { banks, mirroring: Mirroring::SingleScreenA }
    }
}

impl Mapper for AxRom {
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
        self.banks.map_prg_32k((val & 0x07) as isize);
        self.mirroring =
            if val & 0x10 != 0 { Mirroring::SingleScreenB } else { Mirroring::SingleScreenA };
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
