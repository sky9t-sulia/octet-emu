//! Mapper 34 — BNROM and NINA-001, two unrelated boards the iNES format
//! cannot tell apart. NINA-001 decodes registers in the $7FFD..=$7FFF window
//! and has CHR banking; BNROM has neither, so the presence of CHR ROM is used
//! to decide which board this is.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct BnRom {
    banks: Banks,
    mirroring: Mirroring,
    nina: bool,
}

impl BnRom {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let nina = !cart.chr_rom.is_empty();
        let mut banks = Banks::new(cart);
        banks.map_prg_32k(0);
        banks.map_chr_8k(0);
        BnRom { banks, mirroring, nina }
    }
}

impl Mapper for BnRom {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if self.nina {
            match addr {
                0x7ffd => self.banks.map_prg_32k((val & 0x01) as isize),
                0x7ffe => self.banks.map_chr_4k(0, (val & 0x0f) as isize),
                0x7fff => self.banks.map_chr_4k(1, (val & 0x0f) as isize),
                _ => {
                    self.banks.write_cpu(addr, val);
                }
            }
            return;
        }
        if addr < 0x8000 {
            self.banks.write_cpu(addr, val);
            return;
        }
        self.banks.map_prg_32k((val & 0x03) as isize);
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
