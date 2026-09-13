//! Mapper 73 — Konami's VRC3, which is Salamander and nothing else. No CHR
//! banking at all and a single 16 KiB PRG window; the whole chip is really the
//! counter, and that one counts *up* until it overflows rather than down to
//! zero. A mode bit narrows it to eight bits, and the acknowledge register
//! hands a spare enable bit back to the live one so an interrupt can re-arm
//! itself without a second write.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Vrc3 {
    banks: Banks,
    mirroring: Mirroring,
    irq_latch: u16,
    irq_counter: u16,
    irq_enabled: bool,
    /// The enable to restore when the interrupt is acknowledged.
    irq_enable_after: bool,
    irq_eight_bit: bool,
    irq_flag: bool,
}

impl Vrc3 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Vrc3 {
            banks: Banks::new(cart),
            mirroring,
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_enable_after: false,
            irq_eight_bit: false,
            irq_flag: false,
        };
        board.banks.map_prg_16k(0, 0);
        board.banks.map_prg_16k(1, -1);
        board.banks.map_chr_8k(0);
        board
    }
}

impl Mapper for Vrc3 {
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
        let nibble = (val & 0x0f) as u16;
        match addr & 0xf000 {
            0x8000 => self.irq_latch = (self.irq_latch & 0xfff0) | nibble,
            0x9000 => self.irq_latch = (self.irq_latch & 0xff0f) | (nibble << 4),
            0xa000 => self.irq_latch = (self.irq_latch & 0xf0ff) | (nibble << 8),
            0xb000 => self.irq_latch = (self.irq_latch & 0x0fff) | (nibble << 12),
            0xc000 => {
                self.irq_enable_after = val & 1 != 0;
                self.irq_enabled = val & 2 != 0;
                self.irq_eight_bit = val & 4 != 0;
                if self.irq_enabled {
                    self.irq_counter = self.irq_latch;
                }
                self.irq_flag = false;
            }
            0xd000 => {
                self.irq_flag = false;
                self.irq_enabled = self.irq_enable_after;
            }
            0xf000 => self.banks.map_prg_16k(0, (val & 7) as isize),
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
        if self.irq_eight_bit {
            // Only the low byte counts, and only it overflows.
            let low = (self.irq_counter as u8).wrapping_add(1);
            if low == 0 {
                self.irq_flag = true;
                self.irq_counter = (self.irq_counter & 0xff00) | (self.irq_latch & 0x00ff);
            } else {
                self.irq_counter = (self.irq_counter & 0xff00) | low as u16;
            }
        } else if self.irq_counter == 0xffff {
            self.irq_flag = true;
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter += 1;
        }
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
