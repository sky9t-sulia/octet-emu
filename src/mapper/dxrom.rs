//! Mapper 206 — the Namco 108 and the boards around it, DxROM and Tengen's
//! MIMIC-1. This is the MMC3's ancestor and its plainer half: the same pair of
//! registers at `$8000`, the same six banks behind them, and nothing else at
//! all. There is no interrupt counter, no mirroring register and no work RAM,
//! and the top 16 KiB of the map is nailed down rather than swappable.
//!
//! Three boards take the same chip further by wiring a spare line somewhere
//! useful, and each got its own mapper number for it. Mapper 88 runs PPU A12
//! into CHR A16, so the upper pattern table is fetched from the second half of
//! a 128 KiB ROM. Mapper 95 runs CHR A15 into CIRAM A10 instead, and picks the
//! nametable with the same register that picks the pattern. Mapper 154 does
//! what 88 does and adds a one-screen bit to the register at `$8000`.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Board {
    /// Mapper 206, with nothing wired anywhere.
    Plain,
    /// Mapper 88: the upper pattern table comes from the second 64 KiB.
    Split,
    /// Mapper 95: a pattern bank line chooses the nametable.
    Screens,
    /// Mapper 154: the split of 88, and one screen chosen at `$8000`.
    SplitOneScreen,
}

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct DxRom {
    banks: Banks,
    board: Board,
    bank_select: u8,
    regs: [u8; 8],
    mirroring: Mirroring,
}

impl DxRom {
    pub fn new(cart: Cartridge, id: u16) -> Self {
        let mirroring = cart.mirroring;
        let board = match id {
            88 => Board::Split,
            95 => Board::Screens,
            154 => Board::SplitOneScreen,
            _ => Board::Plain,
        };
        let mut board =
            DxRom { banks: Banks::new(cart), board, bank_select: 0, regs: [0; 8], mirroring };
        board.sync();
        board
    }

    fn sync(&mut self) {
        // Four bits of PRG and six of CHR is all the chip carries, which is
        // why these boards stop at 128 KiB of program and 64 of pattern.
        self.banks.map_prg_8k(0, (self.regs[6] & 0x0f) as isize);
        self.banks.map_prg_8k(1, (self.regs[7] & 0x0f) as isize);
        self.banks.map_prg_8k(2, -2);
        self.banks.map_prg_8k(3, -1);

        // On mapper 95 the top bank line is spent on the nametables, so a
        // bank is five bits rather than six.
        let mask = if self.board == Board::Screens { 0x1f } else { 0x3f };
        // On the split boards the upper table is fetched a bank line higher.
        let high = if matches!(self.board, Board::Split | Board::SplitOneScreen) { 0x40 } else { 0 };
        self.banks.map_chr_2k(0, ((self.regs[0] & mask & 0x3e) >> 1) as isize);
        self.banks.map_chr_2k(1, ((self.regs[1] & mask & 0x3e) >> 1) as isize);
        for i in 0..4 {
            self.banks.map_chr_1k(4 + i, ((self.regs[2 + i] & mask) | high) as isize);
        }

        if self.board == Board::Screens {
            // The two 2 KiB registers cover the four quadrants in pairs, the
            // same way the TxSROM's do.
            let a = self.regs[0] & 0x20 != 0;
            let b = self.regs[1] & 0x20 != 0;
            self.mirroring = Mirroring::Quadrants([a, a, b, b]);
        }
    }
}

impl Mapper for DxRom {
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
        // Only the one pair of registers is decoded, and only below $A000.
        match addr & 0xe001 {
            0x8000 => {
                self.bank_select = val & 7;
                if self.board == Board::SplitOneScreen {
                    self.mirroring = if val & 0x40 != 0 {
                        Mirroring::SingleScreenB
                    } else {
                        Mirroring::SingleScreenA
                    };
                }
            }
            0x8001 => {
                self.regs[self.bank_select as usize] = val;
                self.sync();
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

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
