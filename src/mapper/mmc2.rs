//! Mappers 9 and 10 — MMC2 and MMC4, the chips behind Punch-Out!! and Fire
//! Emblem.
//!
//! Their trick is a pair of latches that watch the pattern fetches themselves.
//! Reading a tile whose low address ends at `$FD8` or `$FE8` chooses which of
//! two 4 KiB banks that half of the pattern table will use, and the switch
//! lands after the fetch that caused it — so the tile that triggers it is
//! still drawn from the old bank. A screen can then hold twice the tiles with
//! the processor touching nothing, which is how Little Mac's opponents are
//! drawn as large as they are.
//!
//! The two chips differ in almost nothing else: the MMC2 switches an 8 KiB
//! window and keeps the last three fixed, the MMC4 switches 16 KiB and keeps
//! the last, and the MMC2's left-hand table answers only on the tile's top
//! row where every other case answers on all eight.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Mmc2 {
    banks: Banks,
    /// Mapper 10: a wider PRG window, and both tables latch on every row.
    mmc4: bool,
    prg: u8,
    /// Per half of the pattern table, the bank for each of the two latches.
    chr: [[u8; 2]; 2],
    /// False is the `$FD` bank, true the `$FE` one.
    latch: [bool; 2],
    mirroring: Mirroring,
}

impl Mmc2 {
    pub fn new(cart: Cartridge, id: u16) -> Self {
        let mirroring = cart.mirroring;
        let mut board = Mmc2 {
            banks: Banks::new(cart),
            mmc4: id == 10,
            prg: 0,
            chr: [[0; 2]; 2],
            latch: [false; 2],
            mirroring,
        };
        board.sync();
        board
    }

    fn sync(&mut self) {
        if self.mmc4 {
            self.banks.map_prg_16k(0, (self.prg & 0x0f) as isize);
            self.banks.map_prg_16k(1, -1);
        } else {
            self.banks.map_prg_8k(0, (self.prg & 0x0f) as isize);
            self.banks.map_prg_8k(1, -3);
            self.banks.map_prg_8k(2, -2);
            self.banks.map_prg_8k(3, -1);
        }
        for half in 0..2 {
            let bank = self.chr[half][self.latch[half] as usize];
            self.banks.map_chr_4k(half, (bank & 0x1f) as isize);
        }
    }
}

impl Mapper for Mmc2 {
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
        match addr & 0xf000 {
            0xa000 => {
                self.prg = val;
                self.sync();
            }
            0xb000 => {
                self.chr[0][0] = val;
                self.sync();
            }
            0xc000 => {
                self.chr[0][1] = val;
                self.sync();
            }
            0xd000 => {
                self.chr[1][0] = val;
                self.sync();
            }
            0xe000 => {
                self.chr[1][1] = val;
                self.sync();
            }
            0xf000 => {
                self.mirroring =
                    if val & 1 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical };
            }
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        let val = self.banks.read_ppu(addr);
        let half = ((addr >> 12) & 1) as usize;
        let tile = addr & 0x0fff;
        // The MMC2's left table answers to one address; everything else
        // answers to the whole eight-byte row of the tile.
        let ranged = self.mmc4 || half == 1;
        let hits = |base: u16| tile == base || (ranged && tile > base && tile <= base + 7);
        let want = if hits(0x0fd8) {
            Some(false)
        } else if hits(0x0fe8) {
            Some(true)
        } else {
            None
        };
        // Applied after the read above, which is the whole point of it.
        if let Some(want) = want {
            if self.latch[half] != want {
                self.latch[half] = want;
                self.sync();
            }
        }
        val
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
