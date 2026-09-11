//! Mapper 1 — MMC1.
//!
//! Registers are written a bit at a time: five writes with bit 0 carrying the
//! data shift a value into an internal register, and the fifth write commits
//! it to whichever of the four registers the *last* address selected.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Mmc1 {
    banks: Banks,
    shift: u8,
    /// Counts bits shifted in; the commit happens on the fifth.
    count: u8,

    control: u8,
    chr0: u8,
    chr1: u8,
    prg: u8,

    /// CPU cycle of the last write. Two writes on consecutive cycles — which
    /// is what a read-modify-write produces — have the second one ignored.
    cycle: u64,
    last_write_cycle: u64,

    large_prg: bool,
}

impl Mmc1 {
    pub fn new(cart: Cartridge) -> Self {
        let large_prg = cart.prg_rom.len() > 256 * 1024;
        let mut mmc1 = Mmc1 {
            banks: Banks::new(cart),
            shift: 0,
            count: 0,
            // Power-on control has the PRG mode fixing the last bank at $C000,
            // which is what lets a reset vector be found at all.
            control: 0x0c,
            chr0: 0,
            chr1: 0,
            prg: 0,
            cycle: 0,
            last_write_cycle: u64::MAX,
            large_prg,
        };
        mmc1.sync();
        mmc1
    }

    fn mirroring_bits(&self) -> Mirroring {
        match self.control & 3 {
            0 => Mirroring::SingleScreenA,
            1 => Mirroring::SingleScreenB,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal,
        }
    }

    fn sync(&mut self) {
        // On 512 KiB boards the high CHR bit doubles as a PRG page select,
        // splitting the ROM into two independent 256 KiB halves.
        let prg_base = if self.large_prg && self.chr0 & 0x10 != 0 { 16 } else { 0 };
        let bank = (self.prg & 0x0f) as isize;

        match (self.control >> 2) & 3 {
            // 32 KiB mode ignores the low bit of the bank number.
            0 | 1 => self.banks.map_prg_32k((prg_base + (bank & !1)) / 2),
            2 => {
                // First bank fixed at $8000.
                self.banks.map_prg_16k(0, prg_base);
                self.banks.map_prg_16k(1, prg_base + bank);
            }
            _ => {
                self.banks.map_prg_16k(0, prg_base + bank);
                // Last bank of this 256 KiB page fixed at $C000.
                self.banks.map_prg_16k(1, prg_base + 15);
            }
        }

        if self.control & 0x10 != 0 {
            self.banks.map_chr_4k(0, (self.chr0 & 0x1f) as isize);
            self.banks.map_chr_4k(1, (self.chr1 & 0x1f) as isize);
        } else {
            self.banks.map_chr_8k(((self.chr0 & 0x1e) >> 1) as isize);
        }

        // Bit 4 of the PRG register disables the cartridge's work RAM.
        let ram_disabled = self.prg & 0x10 != 0;
        self.banks.map_wram(if ram_disabled { None } else { Some(0) }, false);
    }
}

impl Mapper for Mmc1 {
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
        // An RMW instruction writes twice back to back; the MMC1 sees only
        // the first, because its shift register is still settling.
        if self.cycle == self.last_write_cycle.wrapping_add(1) {
            self.last_write_cycle = self.cycle;
            return;
        }
        self.last_write_cycle = self.cycle;

        if val & 0x80 != 0 {
            // Reset: clear the shift register and force the PRG mode that
            // fixes the last bank at $C000.
            self.shift = 0;
            self.count = 0;
            self.control |= 0x0c;
            self.sync();
            return;
        }

        self.shift |= (val & 1) << self.count;
        self.count += 1;
        if self.count < 5 {
            return;
        }
        let value = self.shift;
        self.shift = 0;
        self.count = 0;

        match (addr >> 13) & 3 {
            0 => self.control = value,
            1 => self.chr0 = value,
            2 => self.chr1 = value,
            _ => self.prg = value,
        }
        self.sync();
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.banks.read_ppu(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.banks.write_ppu(addr, val);
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring_bits()
    }

    fn ticks_cpu(&self) -> bool {
        true
    }

    fn tick_cpu(&mut self) {
        self.cycle += 1;
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
