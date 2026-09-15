//! Mapper 19 — Namco 163. Eight 1 KiB pattern banks, three switchable 8 KiB
//! program windows, a counter that interrupts off the processor's own clock,
//! and nametables that can come from either the console's memory or the
//! pattern ROM, a quadrant at a time.
//!
//! The board also carries eight wavetable voices of its own. They are not
//! here: the 128 bytes they are programmed through are kept, so a driver
//! writing to them reads back what it wrote, but nothing is sounded. A
//! cartridge that uses them plays with its extra voices missing and nothing
//! else wrong.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

/// The first bank value that means the console's own memory rather than the
/// pattern ROM. Whether it is honoured is up to the two bits in `$E800`.
const CIRAM_FIRST: u8 = 0xe0;

/// The counter interrupts when it reaches this, and stops there.
const IRQ_TOP: u16 = 0x7fff;

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Namco163 {
    banks: Banks,
    /// The eight 1 KiB banks behind the pattern tables.
    chr: [u8; 8],
    /// The four banks behind the nametables.
    nt: [u8; 4],
    /// `$E800` bits 6 and 7: with either set, the half of the pattern table it
    /// speaks for takes `$E0` and above as ordinary banks rather than as the
    /// console's memory.
    chr_ram_off: u8,

    irq_counter: u16,
    irq_enabled: bool,
    irq_flag: bool,

    /// The voices' 128 bytes, and the pointer `$F800` sets into them. Bit 7 of
    /// the pointer asks for it to step on after every access.
    #[cfg_attr(feature = "snapshot", serde(with = "crate::snapshot::bytes"))]
    sound_ram: [u8; 128],
    sound_addr: u8,
    /// `$F800` also carries the write protection for the cartridge's own RAM.
    protect: u8,
}

impl Namco163 {
    pub fn new(cart: Cartridge) -> Self {
        let mut board = Namco163 {
            banks: Banks::new(cart),
            chr: [0; 8],
            nt: [0; 4],
            chr_ram_off: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_flag: false,
            sound_ram: [0; 128],
            sound_addr: 0,
            protect: 0,
        };
        for slot in 0..3 {
            board.banks.map_prg_8k(slot, slot as isize);
        }
        board.banks.map_prg_8k(3, -1);
        board.sync_chr();
        board.banks.map_wram(Some(0), false);
        board
    }

    /// Put the eight pattern banks where their registers say.
    ///
    /// A register naming the console's memory is left pointing wherever it
    /// last pointed: what it reads is answered elsewhere, and the bank it
    /// holds would be meaningless.
    fn sync_chr(&mut self) {
        for slot in 0..8 {
            if !self.chr_is_ciram(slot) {
                self.banks.map_chr_1k(slot, self.chr[slot] as isize);
            }
        }
    }

    /// Whether a pattern slot is answered by the console's own memory.
    fn chr_is_ciram(&self, slot: usize) -> bool {
        let half = if slot < 4 { 0x40 } else { 0x80 };
        self.chr[slot] >= CIRAM_FIRST && self.chr_ram_off & half == 0
    }

    /// Whether the cartridge's RAM will take a 'write' at this address.
    ///
    /// `$F800` opens it only with `$40` in its top nibble, and then each of
    /// the low four bits shuts one 2 KiB quarter again.
    fn wram_open(&self, addr: u16) -> bool {
        if self.protect & 0xf0 != 0x40 {
            return false;
        }
        let quarter = ((addr >> 11) & 3) as u8;
        self.protect & (1 << quarter) == 0
    }
}

impl Mapper for Namco163 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        match addr {
            0x4800..=0x4fff => Some(self.sound_ram[(self.sound_addr & 0x7f) as usize]),
            0x5000..=0x57ff => Some(self.irq_counter as u8),
            0x5800..=0x5fff => {
                Some((self.irq_counter >> 8) as u8 | if self.irq_enabled { 0x80 } else { 0 })
            }
            _ => self.banks.read_cpu(addr),
        }
    }

    fn cpu_read(&mut self, addr: u16) -> Option<u8> {
        let val = self.cpu_peek(addr);
        if matches!(addr, 0x4800..=0x4fff) && self.sound_addr & 0x80 != 0 {
            self.sound_addr = 0x80 | (self.sound_addr.wrapping_add(1) & 0x7f);
        }
        val
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x4800..=0x4fff => {
                self.sound_ram[(self.sound_addr & 0x7f) as usize] = val;
                if self.sound_addr & 0x80 != 0 {
                    self.sound_addr = 0x80 | (self.sound_addr.wrapping_add(1) & 0x7f);
                }
            }
            0x5000..=0x57ff => {
                self.irq_counter = (self.irq_counter & 0xff00) | val as u16;
                self.irq_flag = false;
            }
            0x5800..=0x5fff => {
                self.irq_counter = (self.irq_counter & 0x00ff) | ((val as u16 & 0x7f) << 8);
                self.irq_enabled = val & 0x80 != 0;
                self.irq_flag = false;
            }
            0x6000..=0x7fff => {
                if self.wram_open(addr) {
                    self.banks.write_cpu(addr, val);
                }
            }
            0x8000..=0xbfff => {
                let slot = ((addr - 0x8000) >> 11) as usize;
                self.chr[slot] = val;
                self.sync_chr();
            }
            0xc000..=0xdfff => self.nt[((addr - 0xc000) >> 11) as usize] = val,
            0xe000..=0xe7ff => self.banks.map_prg_8k(0, (val & 0x3f) as isize),
            0xe800..=0xefff => {
                self.banks.map_prg_8k(1, (val & 0x3f) as isize);
                self.chr_ram_off = val & 0xc0;
                self.sync_chr();
            }
            0xf000..=0xf7ff => self.banks.map_prg_8k(2, (val & 0x3f) as isize),
            0xf800..=0xffff => {
                self.protect = val;
                self.sound_addr = val;
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

    /// Each quadrant takes the console's memory or the pattern ROM on its own
    /// account. The ones that take memory say which page here; the ones that
    /// do not are answered by `read_nametable` and what is said for them is
    /// never looked at.
    fn mirroring(&self) -> Mirroring {
        let page = |i: usize| self.nt[i] >= CIRAM_FIRST && self.nt[i] & 1 != 0;
        Mirroring::Quadrants([page(0), page(1), page(2), page(3)])
    }

    fn serves_nametables(&self) -> bool {
        true
    }

    fn read_nametable(&mut self, addr: u16) -> Option<u8> {
        let quadrant = ((addr >> 10) & 3) as usize;
        let bank = self.nt[quadrant];
        if bank >= CIRAM_FIRST {
            return None;
        }
        let len = self.banks.chr_rom.len();
        if len == 0 {
            return Some(0);
        }
        let off = (bank as usize * 1024) % len + (addr as usize & 0x3ff);
        Some(self.banks.chr_rom.get(off).copied().unwrap_or(0))
    }

    fn write_nametable(&mut self, addr: u16, _val: u8) -> bool {
        // A quadrant drawn from the pattern ROM has nothing to write to.
        self.nt[((addr >> 10) & 3) as usize] < CIRAM_FIRST
    }

    fn irq(&self) -> bool {
        self.irq_flag
    }

    fn ticks_cpu(&self) -> bool {
        true
    }

    /// The counter runs off the processor's clock and stops at the top, where
    /// it holds the interrupt until something writes a smaller number.
    fn tick_cpu(&mut self) {
        if !self.irq_enabled || self.irq_counter >= IRQ_TOP {
            return;
        }
        self.irq_counter += 1;
        if self.irq_counter == IRQ_TOP {
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
