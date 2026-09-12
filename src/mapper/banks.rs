//! Shared PRG/CHR window mapping.
//!
//! Almost every mapper is the same machine: four 8 KiB PRG windows over
//! `$8000...=$FFFF`, eight 1 KiB CHR windows over `$0000...=$1FFF`, and some
//! mapper-specific register decoding that moves them. `Banks` owns the ROM,
//! the RAM and the window table so each mapper only has to write the decoder.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use crate::cartridge::Cartridge;

/// Which memory a PRG window points at.
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PrgKind {
    Rom,
    Ram,
}

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Banks {
    #[cfg_attr(feature = "snapshot", serde(skip))]
    pub prg_rom: Vec<u8>,
    pub prg_ram: Vec<u8>,
    #[cfg_attr(feature = "snapshot", serde(skip))]
    pub chr_rom: Vec<u8>,
    pub chr_ram: Vec<u8>,

    /// Byte offset into the target memory for each 8 KiB CPU window at
    /// `$8000`, `$A000`, `$C000`, `$E000`.
    prg_off: [usize; 4],
    prg_kind: [PrgKind; 4],
    /// Byte offset for each 1 KiB PPU window across `$0000...=$1FFF`.
    chr_off: [usize; 8],
    /// Windows the mapper has marked read-only even though CHR RAM backs them.
    chr_ro: [bool; 8],

    /// Byte offset for the 8 KiB window at `$6000`, or `None` when unmapped.
    wram_off: Option<usize>,
    wram_ro: bool,
    /// Length of the battery-backed prefix of `prg_ram`, saved between runs.
    nvram_len: usize,
}

impl Banks {
    /// Take the cartridge's own memory back from `had`, which a state loaded
    /// from a file does not carry: the ROM is on the cartridge, not in the
    /// save, and would otherwise come back empty.
    #[cfg(feature = "snapshot")]
    pub fn keep_rom_of(&mut self, had: &mut Banks) {
        self.prg_rom = core::mem::take(&mut had.prg_rom);
        self.chr_rom = core::mem::take(&mut had.chr_rom);
    }

    pub fn new(cart: Cartridge) -> Self {
        // Battery-backed and volatile PRG RAM occupy one address window, so
        // join them into a single buffer; `nvram_len` marks the saved prefix.
        let mut prg_ram = cart.prg_nvram;
        let nvram_len = prg_ram.len();
        prg_ram.extend_from_slice(&cart.prg_ram);
        if prg_ram.is_empty() {
            prg_ram = vec![0; 8192];
        }

        let mut banks = Banks {
            prg_rom: cart.prg_rom,
            prg_ram,
            chr_rom: cart.chr_rom,
            chr_ram: cart.chr_ram,
            prg_off: [0; 4],
            prg_kind: [PrgKind::Rom; 4],
            chr_off: [0; 8],
            chr_ro: [false; 8],
            wram_off: Some(0),
            wram_ro: false,
            nvram_len,
        };
        // Power-on default: the last 32 KiB of PRG and the first 8 KiB of CHR.
        banks.map_prg_32k(-1);
        banks.map_chr_8k(0);
        banks
    }

    // ---- PRG ------------------------------------------------------------

    /// Map one 8 KiB PRG ROM window. `slot` is 0...=3 (`$8000` + slot * 8 KiB);
    /// `bank` counts 8 KiB units and wraps, negative counting from the end.
    pub fn map_prg_8k(&mut self, slot: usize, bank: isize) {
        let count = (self.prg_rom.len() / 8192).max(1);
        self.prg_off[slot] = wrap_bank(bank, count) * 8192;
        self.prg_kind[slot] = PrgKind::Rom;
    }

    pub fn map_prg_16k(&mut self, slot: usize, bank: isize) {
        self.map_prg_8k(slot * 2, bank * 2);
        self.map_prg_8k(slot * 2 + 1, bank * 2 + 1);
    }

    pub fn map_prg_32k(&mut self, bank: isize) {
        for i in 0..4 {
            self.map_prg_8k(i, bank * 4 + i as isize);
        }
    }

    /// Point a PRG window at cartridge RAM instead of ROM (SunSoft, MMC5, …).
    pub fn map_prg_8k_ram(&mut self, slot: usize, bank: isize) {
        let count = (self.prg_ram.len() / 8192).max(1);
        self.prg_off[slot] = wrap_bank(bank, count) * 8192;
        self.prg_kind[slot] = PrgKind::Ram;
    }

    /// Select the 8 KiB bank visible at `$6000`, or unmap the window.
    pub fn map_wram(&mut self, bank: Option<isize>, read_only: bool) {
        self.wram_off = bank.map(|b| {
            let count = (self.prg_ram.len() / 8192).max(1);
            wrap_bank(b, count) * 8192
        });
        self.wram_ro = read_only;
    }

    pub fn read_cpu(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7fff => {
                let off = self.wram_off?;
                self.prg_ram.get(off + (addr as usize & 0x1fff)).copied()
            }
            0x8000..=0xffff => {
                let slot = (addr as usize >> 13) & 3;
                let off = self.prg_off[slot] + (addr as usize & 0x1fff);
                match self.prg_kind[slot] {
                    PrgKind::Rom => self.prg_rom.get(off).copied(),
                    PrgKind::Ram => self.prg_ram.get(off).copied(),
                }
            }
            _ => None,
        }
    }

    /// Writes that land in cartridge RAM. Returns `false` if the address is
    /// ROM or unmapped, letting the mapper treat it as a register write.
    pub fn write_cpu(&mut self, addr: u16, val: u8) -> bool {
        match addr {
            0x6000..=0x7fff => {
                if self.wram_ro {
                    return true;
                }
                let Some(off) = self.wram_off else { return false };
                if let Some(cell) = self.prg_ram.get_mut(off + (addr as usize & 0x1fff)) {
                    *cell = val;
                }
                true
            }
            0x8000..=0xffff => {
                let slot = (addr as usize >> 13) & 3;
                if self.prg_kind[slot] != PrgKind::Ram {
                    return false;
                }
                let off = self.prg_off[slot] + (addr as usize & 0x1fff);
                if let Some(cell) = self.prg_ram.get_mut(off) {
                    *cell = val;
                }
                true
            }
            _ => false,
        }
    }

    // ---- CHR ------------------------------------------------------------

    /// Map one 1 KiB CHR window. `slot` is 0..=7.
    pub fn map_chr_1k(&mut self, slot: usize, bank: isize) {
        let len = if self.chr_rom.is_empty() { self.chr_ram.len() } else { self.chr_rom.len() };
        let count = (len / 1024).max(1);
        self.chr_off[slot] = wrap_bank(bank, count) * 1024;
    }

    pub fn map_chr_2k(&mut self, slot: usize, bank: isize) {
        for i in 0..2 {
            self.map_chr_1k(slot * 2 + i, bank * 2 + i as isize);
        }
    }

    pub fn map_chr_4k(&mut self, slot: usize, bank: isize) {
        for i in 0..4 {
            self.map_chr_1k(slot * 4 + i, bank * 4 + i as isize);
        }
    }

    pub fn map_chr_8k(&mut self, bank: isize) {
        for i in 0..8 {
            self.map_chr_1k(i, bank * 8 + i as isize);
        }
    }

    pub fn read_ppu(&self, addr: u16) -> u8 {
        let off = self.chr_off[(addr as usize >> 10) & 7] + (addr as usize & 0x3ff);
        let mem = if self.chr_rom.is_empty() { &self.chr_ram } else { &self.chr_rom };
        mem.get(off).copied().unwrap_or(0)
    }

    pub fn write_ppu(&mut self, addr: u16, val: u8) {
        let slot = (addr as usize >> 10) & 7;
        if self.chr_ro[slot] || !self.chr_rom.is_empty() {
            return;
        }
        let off = self.chr_off[slot] + (addr as usize & 0x3ff);
        if let Some(cell) = self.chr_ram.get_mut(off) {
            *cell = val;
        }
    }

    // ---- Save data ------------------------------------------------------

    /// The part of `prg_ram` a battery held: what a save file is made of.
    pub fn nvram(&self) -> &[u8] {
        &self.prg_ram[..self.nvram_len]
    }

    pub fn nvram_mut(&mut self) -> &mut [u8] {
        &mut self.prg_ram[..self.nvram_len]
    }
}

/// Reduce a possibly-negative bank index into `0...count`, where -1 is the last.
fn wrap_bank(bank: isize, count: usize) -> usize {
    let count = count as isize;
    (((bank % count) + count) % count) as usize
}
