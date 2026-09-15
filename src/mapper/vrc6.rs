//! Mappers 24 and 26 — Konami's VRC6. A 16 KiB program window and an 8 KiB
//! one, eight 1 KiB pattern banks, the counter every VRC board shares, and
//! three extra voices.
//!
//! The two numbers are one chip with two address lines crossed: mapper 24 is
//! the VRC6a of Akumajou Densetsu, mapper 26 the VRC6b of Madara and Esper
//! Dream 2, and on the latter the two low address bits of a register arrive
//! the other way round.
//!
//! The voices are not here. A cartridge that uses them plays with its extra
//! three missing and nothing else wrong.

use super::vrc_irq::VrcIrq;
use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Vrc6 {
    banks: Banks,
    /// Whether the two low address bits of a register arrive crossed, which
    /// is the whole of the difference between the two mapper numbers.
    crossed: bool,
    /// The eight pattern-bank registers, R0 through R7.
    chr: [u8; 8],
    /// `$B003`: banking mode in bits 0-1, mirroring in 2-3, where the
    /// nametables come from in 4, how CHR A10 is driven in 5, and whether the
    /// cartridge's RAM answers in 7.
    control: u8,
    irq: VrcIrq,
}

impl Vrc6 {
    pub fn new(cart: Cartridge, id: u16) -> Self {
        let mut board = Vrc6 {
            banks: Banks::new(cart),
            crossed: id == 26,
            chr: [0; 8],
            // Bit 5 set, which is where the commercial boards leave it and
            // the only setting a real cartridge was ever wired for.
            control: 0x20,
            irq: VrcIrq::default(),
        };
        board.banks.map_prg_16k(0, 0);
        board.banks.map_prg_8k(2, -2);
        board.banks.map_prg_8k(3, -1);
        board.banks.map_wram(Some(0), false);
        board.sync_chr();
        board
    }

    /// The register an address names, with the crossed lines undone.
    fn port(&self, addr: u16) -> u16 {
        let low = addr & 3;
        let low = if self.crossed {
            match low {
                1 => 2,
                2 => 1,
                other => other,
            }
        } else {
            low
        };
        (addr & 0xf000) | low
    }

    /// Put a 2 KiB pattern bank across two 1 KiB slots.
    ///
    /// With bit 5 of `$B003` set the low address line comes from the picture
    /// unit, so the register's own lowest bit is ignored and the pair reads as
    /// one 2 KiB bank. With it clear that bit drives the line instead, and
    /// both halves read the same kilobyte.
    fn map_pair(&mut self, slot: usize, reg: u8) {
        let bank = reg as isize;
        if self.control & 0x20 != 0 {
            self.banks.map_chr_1k(slot, bank & !1);
            self.banks.map_chr_1k(slot + 1, bank | 1);
        } else {
            self.banks.map_chr_1k(slot, bank);
            self.banks.map_chr_1k(slot + 1, bank);
        }
    }

    /// Lay the eight pattern slots out the way the banking mode asks.
    fn sync_chr(&mut self) {
        match self.control & 3 {
            // Eight banks of a kilobyte, one register each.
            0 => {
                for slot in 0..8 {
                    self.banks.map_chr_1k(slot, self.chr[slot] as isize);
                }
            }
            // Four banks of two kilobytes, from the first four registers.
            1 => {
                for pair in 0..4 {
                    self.map_pair(pair * 2, self.chr[pair]);
                }
            }
            // The first table in kilobytes, the second in two-kilobyte halves.
            _ => {
                for slot in 0..4 {
                    self.banks.map_chr_1k(slot, self.chr[slot] as isize);
                }
                self.map_pair(4, self.chr[4]);
                self.map_pair(6, self.chr[5]);
            }
        }
    }
}

impl Mapper for Vrc6 {
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
        match self.port(addr) {
            0x8000 => self.banks.map_prg_16k(0, (val & 0x0f) as isize),
            0xc000 => self.banks.map_prg_8k(2, (val & 0x1f) as isize),
            0xb003 => {
                self.control = val;
                self.banks.map_wram(if val & 0x80 != 0 { Some(0) } else { None }, false);
                self.sync_chr();
            }
            port @ 0xd000..=0xd003 => {
                self.chr[(port & 3) as usize] = val;
                self.sync_chr();
            }
            port @ 0xe000..=0xe003 => {
                self.chr[4 + (port & 3) as usize] = val;
                self.sync_chr();
            }
            0xf000 => self.irq.reload = val,
            0xf001 => self.irq.set_control(val),
            0xf002 => self.irq.acknowledge(),
            // $900x, $A00x and $B000-$B002 are the three voices.
            _ => {}
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.banks.read_ppu(addr)
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.banks.write_ppu(addr, val);
    }

    /// Which nametable each quadrant reads, by banking mode.
    ///
    /// Modes 0 and 3 give the four ordinary arrangements, in a different
    /// order from each other. Mode 1 puts one register behind each quadrant,
    /// and mode 2 puts two of them behind pairs; for both, it is the lowest
    /// bit of the register that picks the page.
    fn mirroring(&self) -> Mirroring {
        let page = |reg: u8| self.chr[reg as usize] & 1 != 0;
        match self.control & 3 {
            0 => match (self.control >> 2) & 3 {
                0 => Mirroring::Vertical,
                1 => Mirroring::Horizontal,
                2 => Mirroring::SingleScreenA,
                _ => Mirroring::SingleScreenB,
            },
            1 => Mirroring::Quadrants([page(4), page(5), page(6), page(7)]),
            2 => {
                if (self.control >> 2) & 1 == 0 {
                    Mirroring::Quadrants([page(6), page(7), page(6), page(7)])
                } else {
                    Mirroring::Quadrants([page(6), page(6), page(7), page(7)])
                }
            }
            // The same four arrangements as mode 0, paired the other way.
            _ => match (self.control >> 2) & 3 {
                0 => Mirroring::Horizontal,
                1 => Mirroring::Vertical,
                2 => Mirroring::SingleScreenB,
                _ => Mirroring::SingleScreenA,
            },
        }
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
