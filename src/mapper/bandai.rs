//! Mappers 16 and 159 — Bandai's FCG boards and the LZ93D50 that followed.
//!
//! Eight 1 KiB CHR banks, one 16 KiB PRG window with the last fixed, mirroring
//! of all four kinds, and a counter off the processor's clock. The awkward
//! part is where the registers live: the early boards answer at `$6000`, the
//! later ones at `$8000`, and the same eight offsets repeat through whichever
//! range it is. Nothing else sits at either address on these cartridges, so
//! both are decoded and a game finds its registers wherever it expects them.
//!
//! The later boards also carry a serial EEPROM for saves, clocked a bit at a
//! time through `$800D` and read back through `$6000`. The chip itself is not
//! emulated; its line reads as zero, which a game takes for an EEPROM holding
//! nothing. That is enough to play — both of these sit and spin on that read
//! until something answers — but not to keep anything.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Bandai {
    banks: Banks,
    mirroring: Mirroring,
    /// The FCG-1 and FCG-2 write the counter itself; the LZ93D50 writes a
    /// latch and copies it when the control register is touched.
    latched: bool,
    irq_latch: u16,
    irq_counter: u16,
    irq_enabled: bool,
    irq_flag: bool,
}

impl Bandai {
    pub fn new(cart: Cartridge, submapper: u8) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Bandai {
            banks: Banks::new(cart),
            mirroring,
            latched: submapper != 4,
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_flag: false,
        };
        board.banks.map_prg_16k(0, 0);
        board.banks.map_prg_16k(1, -1);
        board.banks.map_chr_8k(0);
        board
    }

    fn write_register(&mut self, offset: u16, val: u8) {
        match offset {
            0..=7 => self.banks.map_chr_1k(offset as usize, val as isize),
            8 => self.banks.map_prg_16k(0, (val & 0x0f) as isize),
            9 => {
                self.mirroring = match val & 3 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenA,
                    _ => Mirroring::SingleScreenB,
                };
            }
            0x0a => {
                self.irq_enabled = val & 1 != 0;
                self.irq_flag = false;
                if self.latched {
                    self.irq_counter = self.irq_latch;
                }
            }
            0x0b => {
                let half = (self.irq_latch & 0xff00) | val as u16;
                self.irq_latch = half;
                if !self.latched {
                    self.irq_counter = half;
                }
            }
            0x0c => {
                let half = (self.irq_latch & 0x00ff) | ((val as u16) << 8);
                self.irq_latch = half;
                if !self.latched {
                    self.irq_counter = half;
                }
            }
            // $x00D drives the serial EEPROM, which is not emulated.
            _ => {}
        }
    }
}

impl Mapper for Bandai {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        // Bit 4 is the EEPROM's data line, and it answers with nothing to say.
        if (0x6000..0x8000).contains(&addr) {
            return Some(0);
        }
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if addr < 0x6000 {
            return;
        }
        // Both windows are decoded, and the offset repeats every sixteen bytes
        // through each of them.
        self.write_register(addr & 0x000f, val);
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

    fn irq(&self) -> bool {
        self.irq_flag
    }

    fn ticks_cpu(&self) -> bool {
        true
    }

    fn tick_cpu(&mut self) {
        if !self.irq_enabled {
            return;
        }
        // The interrupt comes as the counter passes zero, and it keeps going
        // round from there.
        if self.irq_counter == 0 {
            self.irq_flag = true;
            self.irq_counter = 0xffff;
        } else {
            self.irq_counter -= 1;
        }
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
