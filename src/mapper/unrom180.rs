//! Mapper 180 — the Crazy Climber board. UxROM wired backwards: the fixed
//! bank sits at $8000 and the switchable one at $C000.

use super::{bus_conflict, Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct UnRom180 {
    banks: Banks,
    mirroring: Mirroring,
}

impl UnRom180 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut banks = Banks::new(cart);
        banks.map_prg_16k(0, 0);
        banks.map_prg_16k(1, 0);
        banks.map_chr_8k(0);
        UnRom180 { banks, mirroring }
    }
}

impl Mapper for UnRom180 {
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
        self.banks.map_prg_16k(1, (val & 0x07) as isize);
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
