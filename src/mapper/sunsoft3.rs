//! Mapper 67 — Sunsoft-3. A 16 KiB PRG window with the last bank fixed, four
//! 2 KiB CHR banks, and a sixteen-bit counter off the processor's clock.
//!
//! The counter is loaded through a single address a byte at a time, high half
//! first, with the chip itself remembering which half comes next. Writing to
//! the control register puts that back to the high half, which is how a game
//! keeps the two in step.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Sunsoft3 {
    banks: Banks,
    mirroring: Mirroring,
    irq_counter: u16,
    /// Which half of the counter the next write to `$C800` fills.
    irq_low_half: bool,
    irq_enabled: bool,
    irq_flag: bool,
}

impl Sunsoft3 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Sunsoft3 {
            banks: Banks::new(cart),
            mirroring,
            irq_counter: 0,
            irq_low_half: false,
            irq_enabled: false,
            irq_flag: false,
        };
        board.banks.map_prg_16k(0, 0);
        board.banks.map_prg_16k(1, -1);
        board.banks.map_chr_8k(0);
        board
    }
}

impl Mapper for Sunsoft3 {
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
        match addr & 0xf800 {
            0x8800 => self.banks.map_chr_2k(0, val as isize),
            0x9800 => self.banks.map_chr_2k(1, val as isize),
            0xa800 => self.banks.map_chr_2k(2, val as isize),
            0xb800 => self.banks.map_chr_2k(3, val as isize),
            0xc800 => {
                if self.irq_low_half {
                    self.irq_counter = (self.irq_counter & 0xff00) | val as u16;
                } else {
                    self.irq_counter = (self.irq_counter & 0x00ff) | ((val as u16) << 8);
                }
                self.irq_low_half = !self.irq_low_half;
            }
            0xd800 => {
                self.irq_enabled = val & 0x10 != 0;
                self.irq_low_half = false;
                self.irq_flag = false;
            }
            0xe800 => {
                self.mirroring = match val & 3 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenA,
                    _ => Mirroring::SingleScreenB,
                };
            }
            0xf800 => self.banks.map_prg_16k(0, val as isize),
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
        // The interrupt comes from the borrow out of zero, not from zero
        // itself, and the counter stops counting once it has happened.
        if self.irq_counter == 0 {
            self.irq_counter = 0xffff;
            self.irq_enabled = false;
            self.irq_flag = true;
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
