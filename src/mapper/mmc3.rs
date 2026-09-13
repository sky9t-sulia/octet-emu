//! Mapper 4 — MMC3, and the closely related mapper 118 (TxSROM) and 119.
//!
//! The scanline counter is the interesting part. It has no idea what a
//! scanline is: it counts rising edges on PPU address line A12, which during
//! rendering happen once per line as the fetch pattern moves between the
//! background and sprite pattern tables. A low period of at least three dots
//! is required to re-arm it, which is what filters out the many other A12
//! transitions within a line.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Mmc3 {
    banks: Banks,
    variant: Variant,

    /// Last value written to $8000: bank to update, plus the two mode bits.
    bank_select: u8,
    regs: [u8; 8],
    mirroring: Mirroring,
    ram_enabled: bool,
    ram_write_protect: bool,

    irq_counter: u8,
    irq_latch: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_flag: bool,

    /// A12 filtering state: how long the line has been low.
    a12_low_cycles: u16,
    prev_a12: bool,

    /// Nametable select per 1 KiB region, used only by TxSROM.
    chr_nametables: [bool; 8],

    /// MMC6's on-chip work RAM, mirrored four times across $7000-$7FFF with
    /// the low half of each mirror being one bank and the high half the other.
    #[cfg_attr(feature = "snapshot", serde(with = "crate::snapshot::bytes"))]
    mmc6_ram: [u8; 0x400],
    /// Per-half read/write enables from $A001, and the master enable from
    /// bit 5 of $8000.
    mmc6_perms: u8,
}

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// MMC3 revisions B and C, by far the most common.
    Standard,
    /// Revision A: fires only on a genuine transition to zero, so a reload to
    /// zero does not re-trigger every scanline. NES 2.0 submapper 4.
    RevA,
    /// MMC6: an MMC3 with a kilobyte of work RAM on the chip itself, split
    /// into two independently protected halves. NES 2.0 submapper 1.
    Mmc6,
    /// Mapper 118: the high bit of each CHR bank drives nametable selection.
    TxSRom,
    /// Mapper 119: CHR ROM and CHR RAM are both mapped, selected per bank.
    TqSRom,
}

impl Mmc3 {
    pub fn new(cart: Cartridge, id: u16, submapper: u8) -> Self {
        let mirroring = cart.mirroring;
        let variant = match (id, submapper) {
            (118, _) => Variant::TxSRom,
            (119, _) => Variant::TqSRom,
            (_, 1) => Variant::Mmc6,
            (_, 4) => Variant::RevA,
            _ => Variant::Standard,
        };
        let mut mmc3 = Mmc3 {
            banks: Banks::new(cart),
            variant,
            bank_select: 0,
            regs: [0, 2, 4, 5, 6, 7, 0, 1],
            mirroring,
            ram_enabled: true,
            ram_write_protect: false,
            irq_counter: 0,
            irq_latch: 0,
            irq_reload: false,
            irq_enabled: false,
            irq_flag: false,
            a12_low_cycles: 0,
            prev_a12: false,
            chr_nametables: [false; 8],
            mmc6_ram: [0; 0x400],
            mmc6_perms: 0,
        };
        mmc3.sync();
        mmc3
    }

    fn sync(&mut self) {
        // Bit 6 swaps which end of the address space is switchable; the other
        // end holds the second-to-last and last banks.
        if self.bank_select & 0x40 == 0 {
            self.banks.map_prg_8k(0, (self.regs[6] & 0x3f) as isize);
            self.banks.map_prg_8k(2, -2);
        } else {
            self.banks.map_prg_8k(0, -2);
            self.banks.map_prg_8k(2, (self.regs[6] & 0x3f) as isize);
        }
        self.banks.map_prg_8k(1, (self.regs[7] & 0x3f) as isize);
        self.banks.map_prg_8k(3, -1);

        // Bit 7 swaps the 2 KiB and 1 KiB CHR halves.
        let inverted = self.bank_select & 0x80 != 0;
        let (two_kb_base, one_kb_base) = if inverted { (4, 0) } else { (0, 4) };
        self.banks.map_chr_2k(two_kb_base / 2, (self.regs[0] >> 1) as isize);
        self.banks.map_chr_2k(two_kb_base / 2 + 1, (self.regs[1] >> 1) as isize);
        for i in 0..4 {
            self.banks.map_chr_1k(one_kb_base + i, self.regs[2 + i] as isize);
        }

        if self.variant == Variant::TxSRom {
            // Bit 7 of each CHR register picks the nametable for the region
            // that register covers, giving a cheap four-screen effect.
            let nt = |r: u8| r & 0x80 != 0;
            let (a, b) = (nt(self.regs[0]), nt(self.regs[1]));
            let ones = [nt(self.regs[2]), nt(self.regs[3]), nt(self.regs[4]), nt(self.regs[5])];
            if inverted {
                self.chr_nametables = [ones[0], ones[1], ones[2], ones[3], a, a, b, b];
                self.mirroring = Mirroring::Quadrants([ones[0], ones[1], ones[2], ones[3]]);
            } else {
                self.chr_nametables = [a, a, b, b, ones[0], ones[1], ones[2], ones[3]];
                self.mirroring = Mirroring::Quadrants([a, a, b, b]);
            }
        }

        if self.variant == Variant::Mmc6 {
            self.banks.map_wram(None, false);
        } else {
            self.banks
                .map_wram(if self.ram_enabled { Some(0) } else { None }, self.ram_write_protect);
        }
    }

    /// Clock the scanline counter on a filtered A12 rise.
    fn clock_irq(&mut self) {
        let before = self.irq_counter;
        let reloading = self.irq_reload;
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
            self.irq_reload = false;
        } else {
            self.irq_counter -= 1;
        }

        if self.irq_counter != 0 || !self.irq_enabled {
            return;
        }
        match self.variant {
            // Revision A fires only on a real transition to zero. MMC6 shares
            // this, as do some chips marked MMC3B.
            Variant::RevA | Variant::Mmc6 => {
                if before > 0 || reloading {
                    self.irq_flag = true;
                }
            }
            // Revision B reloads to zero and fires again on every clock, which
            // is the behavior most software was written against.
            _ => self.irq_flag = true,
        }
    }

    /// Whether MMC6's work RAM answers at all, and for which half.
    fn mmc6_access(&self, addr: u16) -> (bool, bool) {
        // Bit 5 of $8000 is the master enable; without it neither half response.
        if self.bank_select & 0x20 == 0 {
            return (false, false);
        }
        // The low half of each 1 KiB mirror is one bank, the high half the other.
        if addr & 0x200 == 0 {
            (self.mmc6_perms & 0x20 != 0, self.mmc6_perms & 0x10 != 0)
        } else {
            (self.mmc6_perms & 0x80 != 0, self.mmc6_perms & 0x40 != 0)
        }
    }
}

impl Mapper for Mmc3 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        if self.variant == Variant::Mmc6 && addr < 0x8000 {
            if addr < 0x7000 {
                return None;
            }
            let (readable, _) = self.mmc6_access(addr);
            // A disabled half reads back as zero rather than open bus.
            return Some(if readable { self.mmc6_ram[(addr & 0x3ff) as usize] } else { 0 });
        }
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if self.variant == Variant::Mmc6 && addr < 0x8000 {
            if addr >= 0x7000 {
                let (_, writable) = self.mmc6_access(addr);
                if writable {
                    self.mmc6_ram[(addr & 0x3ff) as usize] = val;
                }
            }
            return;
        }
        if addr < 0x8000 {
            self.banks.write_cpu(addr, val);
            return;
        }
        // Registers are decoded by address bit 13 and the write's parity.
        match (addr & 0xe001, addr) {
            (0x8000, _) => {
                self.bank_select = val;
                self.sync();
            }
            (0x8001, _) => {
                self.regs[(self.bank_select & 7) as usize] = val;
                self.sync();
            }
            (0xa000, _) => {
                if self.variant != Variant::TxSRom {
                    self.mirroring =
                        if val & 1 != 0 { Mirroring::Horizontal } else { Mirroring::Vertical };
                }
            }
            (0xa001, _) => {
                if self.variant == Variant::Mmc6 {
                    // On MMC6 this register carries per-half read and write
                    // enables instead of a single enable and protect bit.
                    self.mmc6_perms = val;
                } else {
                    self.ram_enabled = val & 0x80 != 0;
                    self.ram_write_protect = val & 0x40 != 0;
                    self.sync();
                }
            }
            (0xc000, _) => self.irq_latch = val,
            (0xc001, _) => {
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            (0xe000, _) => {
                self.irq_enabled = false;
                self.irq_flag = false;
            }
            (0xe001, _) => self.irq_enabled = true,
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

    fn read_nametable(&mut self, _addr: u16) -> Option<u8> {
        None
    }

    fn irq(&self) -> bool {
        self.irq_flag
    }

    fn watches_ppu_bus(&self) -> bool {
        true
    }

    fn tick_ppu_addr(&mut self, addr: u16) {
        let a12 = addr & 0x1000 != 0;
        if !a12 {
            self.a12_low_cycles = self.a12_low_cycles.saturating_add(1);
        } else {
            // Ten PPU cycles low before a rise counts. That rejects the
            // four-cycle dips in the sprite fetch phase: one clock per line.
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
