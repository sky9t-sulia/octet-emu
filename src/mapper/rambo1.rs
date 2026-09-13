//! Mapper 64 — Tengen RAMBO-1. The MMC3's shape drawn differently: three of
//! the four PRG windows switch rather than two, the lower pattern table can be
//! four 1 KiB banks instead of two 2 KiB ones, and the counter has a second
//! mode in which the processor clocks it rather than the picture.
//!
//! Sixteen bank registers are addressable and ten are wired: six for CHR, two
//! more for CHR when the 1 KiB mode is on, and three for PRG — the third of
//! which is register fifteen rather than eight, which is the one place the
//! numbering surprises.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Rambo1 {
    banks: Banks,
    /// The last write to `$8000`: which register the next write to `$8001`
    /// fills, and the three mode bits above it.
    bank_select: u8,
    regs: [u8; 16],
    mirroring: Mirroring,

    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_flag: bool,
    irq_reload: bool,
    /// Set through `$C001`: the counter is clocked by the processor rather
    /// than by A12.
    cycle_mode: bool,
    /// Divides the processor's clock by four for that mode.
    prescaler: u8,

    /// A12 filtering for the other mode, as on the MMC3.
    a12_low_cycles: u16,
    prev_a12: bool,
}

impl Rambo1 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Rambo1 {
            banks: Banks::new(cart),
            bank_select: 0,
            regs: [0; 16],
            mirroring,
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_flag: false,
            irq_reload: false,
            cycle_mode: false,
            prescaler: 0,
            a12_low_cycles: 0,
            prev_a12: false,
        };
        board.sync();
        board
    }

    fn sync(&mut self) {
        // Bit 6 decides which end of the map the two odd PRG registers sit at.
        // R7 and the last bank do not move.
        let (first, third) = if self.bank_select & 0x40 == 0 {
            (self.regs[6], self.regs[15])
        } else {
            (self.regs[15], self.regs[6])
        };
        self.banks.map_prg_8k(0, first as isize);
        self.banks.map_prg_8k(1, self.regs[7] as isize);
        self.banks.map_prg_8k(2, third as isize);
        self.banks.map_prg_8k(3, -1);

        // Bit 7 swaps the two pattern tables over. Bit 5 decides whether the
        // lower one is banked in halves or in quarters; the upper one is four
        // 1 KiB banks either way.
        let (low, high) = if self.bank_select & 0x80 == 0 { (0, 4) } else { (4, 0) };
        if self.bank_select & 0x20 == 0 {
            // The low bit of the register is ignored: A10 passes through.
            self.banks.map_chr_2k(low / 2, (self.regs[0] >> 1) as isize);
            self.banks.map_chr_2k(low / 2 + 1, (self.regs[1] >> 1) as isize);
        } else {
            // R8 and R9 fill the quarters R0 and R1 give up.
            self.banks.map_chr_1k(low, self.regs[0] as isize);
            self.banks.map_chr_1k(low + 1, self.regs[8] as isize);
            self.banks.map_chr_1k(low + 2, self.regs[1] as isize);
            self.banks.map_chr_1k(low + 3, self.regs[9] as isize);
        }
        for i in 0..4 {
            self.banks.map_chr_1k(high + i, self.regs[2 + i] as isize);
        }
    }

    /// One clock of the counter, from whichever source is driving it.
    fn clock_irq(&mut self) {
        if self.irq_reload {
            self.irq_reload = false;
            // A reload asked for through `$C001` comes back odd: the chip sets
            // the low bit of any latch that is not zero.
            self.irq_counter = if self.irq_latch == 0 { 0 } else { self.irq_latch | 1 };
        } else if self.irq_counter == 0 {
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter -= 1;
        }
        // The chip holds the line down a few cycles after this, later than the
        // MMC3 does. That delay is not modelled here, as it is not there.
        if self.irq_counter == 0 && self.irq_enabled {
            self.irq_flag = true;
        }
    }
}

impl Mapper for Rambo1 {
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
        // Decoded by address bit 13 and the write's parity, as on the MMC3.
        match addr & 0xe001 {
            0x8000 => {
                self.bank_select = val;
                self.sync();
            }
            0x8001 => {
                self.regs[(self.bank_select & 0x0f) as usize] = val;
                self.sync();
            }
            0xa000 => {
                self.mirroring =
                    if val & 1 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical };
            }
            0xc000 => self.irq_latch = val,
            0xc001 => {
                self.cycle_mode = val & 1 != 0;
                self.irq_counter = 0;
                self.irq_reload = true;
                self.prescaler = 0;
            }
            0xe000 => {
                self.irq_enabled = false;
                self.irq_flag = false;
            }
            0xe001 => self.irq_enabled = true,
            // `$A001` is wired to nothing on this board.
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
        if !self.cycle_mode {
            return;
        }
        self.prescaler += 1;
        if self.prescaler == 4 {
            self.prescaler = 0;
            self.clock_irq();
        }
    }

    fn watches_ppu_bus(&self) -> bool {
        true
    }

    fn tick_ppu_addr(&mut self, addr: u16) {
        if self.cycle_mode {
            return;
        }
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
