//! Mapper 71 — Camerica and Codemasters, the BF9093 and BF9097 boards. The
//! same shape as UxROM: a switchable 16 KiB bank at $8000, the last bank fixed
//! at $C000, and CHR RAM with no banking of its own. Unlike UxROM these have
//! no bus conflicts, so a write reaches the latch whatever the ROM holds.
//!
//! The BF9097 adds one register the BF9093 does not have, and only Fire Hawk
//! uses it: a write to $9000-$9FFF chooses which nametable the single screen
//! comes from. iNES cannot tell the two boards apart and most dumps carry no
//! submapper, so the latch is honoured either way — nothing on a BF9093 writes
//! there, and a game that never writes keeps the mirroring its header asked
//! for.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Bf909x {
    banks: Banks,
    mirroring: Mirroring,
}

impl Bf909x {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut banks = Banks::new(cart);
        banks.map_prg_16k(0, 0);
        banks.map_prg_16k(1, -1);
        banks.map_chr_8k(0);
        Bf909x { banks, mirroring }
    }
}

impl Mapper for Bf909x {
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
            // The mirroring latch of a BF9097 board.
            0x9000..=0x9fff => {
                self.mirroring = if val & 0x10 != 0 {
                    Mirroring::SingleScreenB
                } else {
                    Mirroring::SingleScreenA
                };
            }
            // The bank latch is clocked from the top of the map, not the
            // bottom: only $C000 and up reach it.
            0xc000.. => self.banks.map_prg_16k(0, (val & 0x0f) as isize),
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
