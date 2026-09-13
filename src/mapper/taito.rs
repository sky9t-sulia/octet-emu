//! Mappers 33 and 48 — Taito's TC0190 and the TC0690 built on it.
//!
//! Two 8 KiB PRG windows below and the last 16 KiB fixed above, with the
//! pattern tables split as the MMC3 splits them: two 2 KiB banks and four
//! 1 KiB ones. The bank numbers are not the MMC3's, though — the 2 KiB
//! registers count in 2 KiB units rather than dropping a bit.
//!
//! The TC0690 adds an interrupt counter clocked off A12 exactly as the MMC3's
//! is, with the reload value stored complemented, and moves mirroring out to
//! its own register.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Taito {
    banks: Banks,
    /// Mapper 48: the TC0690, which has the counter and the extra register.
    tc0690: bool,
    prg: [u8; 2],
    chr: [u8; 6],
    mirroring: Mirroring,

    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_flag: bool,
    a12_low_cycles: u16,
    prev_a12: bool,
}

impl Taito {
    pub fn new(cart: Cartridge, id: u16) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Taito {
            banks: Banks::new(cart),
            tc0690: id == 48,
            prg: [0; 2],
            chr: [0; 6],
            mirroring,
            irq_latch: 0,
            irq_counter: 0,
            irq_reload: false,
            irq_enabled: false,
            irq_flag: false,
            a12_low_cycles: 0,
            prev_a12: false,
        };
        board.sync();
        board
    }

    fn sync(&mut self) {
        self.banks.map_prg_8k(0, (self.prg[0] & 0x3f) as isize);
        self.banks.map_prg_8k(1, (self.prg[1] & 0x3f) as isize);
        self.banks.map_prg_8k(2, -2);
        self.banks.map_prg_8k(3, -1);

        self.banks.map_chr_2k(0, self.chr[0] as isize);
        self.banks.map_chr_2k(1, self.chr[1] as isize);
        for i in 0..4 {
            self.banks.map_chr_1k(4 + i, self.chr[2 + i] as isize);
        }
    }

    /// The MMC3's counter, and the same filtered A12 rise drives it.
    fn clock_irq(&mut self) {
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
            self.irq_reload = false;
        } else {
            self.irq_counter -= 1;
        }
        // The chip waits a few processor cycles more than the MMC3 before it
        // pulls the line. That delay is not modelled here.
        if self.irq_counter == 0 && self.irq_enabled {
            self.irq_flag = true;
        }
    }
}

impl Mapper for Taito {
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
        match addr & 0xe003 {
            0x8000 => {
                self.prg[0] = val;
                // On the TC0190 the spare bit carries mirroring; the TC0690
                // has a register for it instead.
                if !self.tc0690 {
                    self.mirroring =
                        if val & 0x40 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical };
                }
                self.sync();
            }
            0x8001 => {
                self.prg[1] = val;
                self.sync();
            }
            0x8002 | 0x8003 => {
                self.chr[(addr & 1) as usize] = val;
                self.sync();
            }
            0xa000..=0xa003 => {
                self.chr[2 + (addr & 3) as usize] = val;
                self.sync();
            }
            0xc000 if self.tc0690 => self.irq_latch = val ^ 0xff,
            0xc001 if self.tc0690 => {
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            0xc002 if self.tc0690 => self.irq_enabled = true,
            0xc003 if self.tc0690 => {
                self.irq_enabled = false;
                self.irq_flag = false;
            }
            0xe000 if self.tc0690 => {
                self.mirroring =
                    if val & 0x40 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical };
            }
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

    fn watches_ppu_bus(&self) -> bool {
        self.tc0690
    }

    fn tick_ppu_addr(&mut self, addr: u16) {
        let a12 = addr & 0x1000 != 0;
        if !a12 {
            self.a12_low_cycles = self.a12_low_cycles.saturating_add(1);
        } else {
            if !self.prev_a12 && self.a12_low_cycles >= 10 {
                self.clock_irq();
            }
            self.a12_low_cycles = 0;
        }
        self.prev_a12 = a12;
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
