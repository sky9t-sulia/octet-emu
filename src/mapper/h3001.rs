//! Mapper 65 — Irem's H3001. Three switchable 8 KiB PRG windows with the last
//! fixed, eight 1 KiB CHR banks, and a counter that runs off the processor
//! rather than the picture: sixteen bits wide, loaded a byte at a time, and it
//! stops dead at zero instead of reloading itself.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct H3001 {
    banks: Banks,
    mirroring: Mirroring,
    irq_latch: u16,
    irq_counter: u16,
    irq_enabled: bool,
    irq_flag: bool,
}

impl H3001 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut board = H3001 {
            banks: Banks::new(cart),
            mirroring,
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_flag: false,
        };
        board.banks.map_prg_8k(0, 0);
        board.banks.map_prg_8k(1, 1);
        board.banks.map_prg_8k(2, -2);
        board.banks.map_prg_8k(3, -1);
        board.banks.map_chr_8k(0);
        board
    }
}

impl Mapper for H3001 {
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
        match addr {
            0x8000 => self.banks.map_prg_8k(0, val as isize),
            0xa000 => self.banks.map_prg_8k(1, val as isize),
            0xc000 => self.banks.map_prg_8k(2, val as isize),
            0x9001 => {
                self.mirroring =
                    if val & 0x80 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical };
            }
            0x9003 => {
                self.irq_enabled = val & 0x80 != 0;
                self.irq_flag = false;
            }
            0x9004 => {
                self.irq_counter = self.irq_latch;
                self.irq_flag = false;
            }
            0x9005 => self.irq_latch = (self.irq_latch & 0x00ff) | ((val as u16) << 8),
            0x9006 => self.irq_latch = (self.irq_latch & 0xff00) | val as u16,
            0xb000..=0xb007 => self.banks.map_chr_1k((addr & 7) as usize, val as isize),
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
        if !self.irq_enabled || self.irq_counter == 0 {
            return;
        }
        self.irq_counter -= 1;
        // Nothing reloads it: the counter sits at zero until $9004 is written.
        if self.irq_counter == 0 {
            self.irq_flag = true;
        }
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
