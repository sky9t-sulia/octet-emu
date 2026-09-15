//! Mapper 85 — Konami's VRC7. Three switchable 8 KiB program windows, eight
//! 1 KiB pattern banks, the counter every VRC board shares, and six voices of
//! frequency modulation.
//!
//! The voices are not here, and on this board that is a real loss rather than
//! a small one: Lagrange Point was built around them and its music is the
//! reason the chip exists. Everything else plays.
//!
//! Two boards carry the chip and they differ only in which address line picks
//! the second register of a pair: Tiny Toon Adventures uses A3, Lagrange Point
//! A4. The header's submapper says which, and a cartridge that does not say
//! is taken as A4, the one with the sound hardware fitted.

use super::vrc_irq::VrcIrq;
use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Vrc7 {
    banks: Banks,
    /// Whether A4 picks the second register of a pair rather than A3.
    a4: bool,
    mirroring: Mirroring,
    irq: VrcIrq,
}

impl Vrc7 {
    pub fn new(cart: Cartridge, submapper: u8) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Vrc7 {
            banks: Banks::new(cart),
            a4: submapper != 1,
            mirroring,
            irq: VrcIrq::default(),
        };
        for slot in 0..3 {
            board.banks.map_prg_8k(slot, slot as isize);
        }
        board.banks.map_prg_8k(3, -1);
        board.banks.map_chr_8k(0);
        board.banks.map_wram(Some(0), false);
        board
    }

    /// Which of a pair of registers an address names.
    fn second(&self, addr: u16) -> bool {
        let line = if self.a4 { 0x10 } else { 0x08 };
        addr & line != 0
    }
}

impl Mapper for Vrc7 {
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
        let second = self.second(addr);
        let bank = (val & 0x3f) as isize;
        match addr & 0xf000 {
            0x8000 => self.banks.map_prg_8k(if second { 1 } else { 0 }, bank),
            // The second register of this pair is the sound port.
            0x9000 if !second => self.banks.map_prg_8k(2, bank),
            0xa000 | 0xb000 | 0xc000 | 0xd000 => {
                let pair = ((addr >> 12) - 0xa) as usize;
                self.banks.map_chr_1k(pair * 2 + second as usize, val as isize);
            }
            0xe000 if !second => {
                self.mirroring = match val & 3 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenA,
                    _ => Mirroring::SingleScreenB,
                };
                self.banks.map_wram(if val & 0x80 != 0 { Some(0) } else { None }, false);
            }
            0xe000 => self.irq.reload = val,
            0xf000 if !second => self.irq.set_control(val),
            0xf000 => self.irq.acknowledge(),
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
        self.irq.flag
    }

    fn ticks_cpu(&self) -> bool {
        true
    }

    fn tick_cpu(&mut self) {
        self.irq.tick();
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
