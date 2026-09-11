//! Konami VRC2 and VRC4 — mappers 21, 22, 23, 25, 27 and 183.
//!
//! One chip family behind six mapper numbers. The registers are identical;
//! what differs is which pair of address lines carries the two low register
//! select bits, because Konami wired the same die onto boards differently.
//! iNES cannot express that, so the mapper number plus submapper picks the
//! wiring, and where the submapper is absent both candidate wirings are ORed
//! together — wrong in theory, right for every game in practice.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Variant {
    Vrc2a,
    Vrc2b,
    Vrc2c,
    Vrc4a,
    Vrc4b,
    Vrc4c,
    Vrc4d,
    Vrc4e,
    Vrc4f,
    Vrc4_27,
    Vrc4_183,
}

impl Variant {
    /// VRC4 adds the PRG mode bit and the IRQ counter; VRC2 has neither.
    fn is_vrc4(self) -> bool {
        self >= Variant::Vrc4a
    }
}

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Vrc2_4 {
    banks: Banks,
    variant: Variant,
    /// True when the submapper did not say which variant this is, so register
    /// addresses are decoded permissively.
    heuristics: bool,

    prg0: u8,
    prg1: u8,
    prg_mode: u8,
    chr_lo: [u8; 8],
    chr_hi: [u8; 8],
    mirroring: Mirroring,

    /// One-bit latch behind the VRC2 microwire interface at $6000.
    latch: u8,
    has_wram: bool,

    irq: VrcIrq,
}

/// The VRC4 interrupt counter. Scanline mode divides the CPU clock by 341/3;
/// cycle mode counts CPU cycles directly.
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Default)]
struct VrcIrq {
    reload: u8,
    counter: u8,
    prescaler: i16,
    enabled: bool,
    enabled_after_ack: bool,
    cycle_mode: bool,
    flag: bool,
}

impl VrcIrq {
    fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        self.prescaler -= 3;
        if self.cycle_mode || self.prescaler <= 0 {
            if self.counter == 0xff {
                self.counter = self.reload;
                self.flag = true;
            } else {
                self.counter += 1;
            }
            self.prescaler += 341;
        }
    }

    fn set_control(&mut self, val: u8) {
        self.enabled_after_ack = val & 0x01 != 0;
        self.enabled = val & 0x02 != 0;
        self.cycle_mode = val & 0x04 != 0;
        if self.enabled {
            self.counter = self.reload;
            self.prescaler = 341;
        }
        self.flag = false;
    }

    /// Acknowledging restores the enable bit that was parked by the control
    /// write, which is how the handler re-arms without a second write.
    fn acknowledge(&mut self) {
        self.enabled = self.enabled_after_ack;
        self.flag = false;
    }
}

impl Vrc2_4 {
    pub fn new(cart: Cartridge, id: u16, submapper: u8) -> Self {
        let variant = match (id, submapper) {
            (21, 2) => Variant::Vrc4c,
            (21, _) => Variant::Vrc4a,
            (22, _) => Variant::Vrc2a,
            (23, 1) => Variant::Vrc4f,
            (23, 2) => Variant::Vrc4e,
            (23, _) => Variant::Vrc2b,
            (25, 2) => Variant::Vrc4d,
            (25, 3) => Variant::Vrc2c,
            (25, _) => Variant::Vrc4b,
            (27, _) => Variant::Vrc4_27,
            (183, _) => Variant::Vrc4_183,
            _ => Variant::Vrc2b,
        };
        let heuristics = submapper == 0 && !matches!(id, 22 | 27 | 183);
        let has_wram = !cart.prg_ram.is_empty() || !cart.prg_nvram.is_empty();
        let mirroring = cart.mirroring;

        let mut vrc = Vrc2_4 {
            banks: Banks::new(cart),
            variant,
            heuristics,
            prg0: 0,
            prg1: 1,
            prg_mode: 0,
            chr_lo: [0; 8],
            chr_hi: [0; 8],
            mirroring,
            latch: 0,
            has_wram,
            irq: VrcIrq::default(),
        };
        vrc.sync();
        vrc
    }

    fn sync(&mut self) {
        for i in 0..8 {
            let mut page = self.chr_lo[i] as isize | ((self.chr_hi[i] as isize) << 4);
            if self.variant == Variant::Vrc2a {
                // VRC2a wires only the high seven bits of the CHR register, so
                // the page number is effectively halved.
                page >>= 1;
            }
            self.banks.map_chr_1k(i, page);
        }

        // The PRG mode bit swaps the switchable and fixed banks between the
        // $8000 and $C000 windows.
        if self.prg_mode == 0 {
            self.banks.map_prg_8k(0, self.prg0 as isize);
            self.banks.map_prg_8k(1, self.prg1 as isize);
            self.banks.map_prg_8k(2, -2);
        } else {
            self.banks.map_prg_8k(0, -2);
            self.banks.map_prg_8k(1, self.prg1 as isize);
            self.banks.map_prg_8k(2, self.prg0 as isize);
        }
        self.banks.map_prg_8k(3, -1);
    }

    /// Fold the board's register-select address lines down to A0 and A1.
    fn translate(&self, addr: u16) -> u16 {
        let (a0, a1) = if self.heuristics {
            match self.variant {
                // Mapper 25: VRC2c and VRC4b share one wiring, VRC4d another.
                Variant::Vrc2c | Variant::Vrc4b | Variant::Vrc4d => {
                    (((addr >> 1) & 1) | ((addr >> 3) & 1), (addr & 1) | ((addr >> 2) & 1))
                }
                // Mapper 21: VRC4a and VRC4c.
                Variant::Vrc4a | Variant::Vrc4c => {
                    (((addr >> 1) & 1) | ((addr >> 6) & 1), ((addr >> 2) & 1) | ((addr >> 7) & 1))
                }
                // Mapper 23: VRC2b, VRC4f and VRC4e.
                _ => ((addr & 1) | ((addr >> 2) & 1), ((addr >> 1) & 1) | ((addr >> 3) & 1)),
            }
        } else {
            match self.variant {
                Variant::Vrc2a | Variant::Vrc2c | Variant::Vrc4b => ((addr >> 1) & 1, addr & 1),
                Variant::Vrc4_27 | Variant::Vrc2b | Variant::Vrc4f => (addr & 1, (addr >> 1) & 1),
                Variant::Vrc4d => ((addr >> 3) & 1, (addr >> 2) & 1),
                Variant::Vrc4a => ((addr >> 1) & 1, (addr >> 2) & 1),
                Variant::Vrc4c => ((addr >> 6) & 1, (addr >> 7) & 1),
                Variant::Vrc4e | Variant::Vrc4_183 => ((addr >> 2) & 1, (addr >> 3) & 1),
            }
        };
        (addr & 0xff00) | (a1 << 1) | a0
    }

    fn set_mirroring(&mut self, val: u8) {
        // A confirmed VRC2 only decodes one bit here; VRC4 decodes two.
        let mask = if !self.heuristics && !self.variant.is_vrc4() { 0x01 } else { 0x03 };
        self.mirroring = match val & mask {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleScreenA,
            _ => Mirroring::SingleScreenB,
        };
    }
}

impl Mapper for Vrc2_4 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        // With no work RAM fitted, $6000-$6FFF is the VRC2 microwire latch.
        if (0x6000..0x7000).contains(&addr) && !self.has_wram && !self.variant.is_vrc4() {
            return Some(self.latch);
        }
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if addr < 0x8000 {
            if !self.has_wram && !self.variant.is_vrc4() {
                self.latch = val & 0x01;
            } else {
                self.banks.write_cpu(addr, val);
            }
            return;
        }

        let addr = self.translate(addr) & 0xf00f;
        match addr {
            0x8000..=0x8006 => self.prg0 = val & 0x1f,
            0x9000..=0x9001 => self.set_mirroring(val),
            // On VRC2 the whole $9000 block selects mirroring; on VRC4 the
            // upper half of it carries the PRG mode bit instead.
            0x9002..=0x9003 => {
                if self.variant.is_vrc4() {
                    self.prg_mode = (val >> 1) & 0x01;
                } else {
                    self.set_mirroring(val);
                }
            }
            0xa000..=0xa006 => self.prg1 = val & 0x1f,
            0xb000..=0xe006 => {
                // Each CHR register is split across two addresses: the low
                // nibble in one, the high five bits in the next.
                let reg = ((((addr >> 12) & 0x07) - 3) << 1) | ((addr >> 1) & 0x01);
                let reg = reg as usize & 7;
                if addr & 0x01 == 0 {
                    self.chr_lo[reg] = val & 0x0f;
                } else {
                    self.chr_hi[reg] = val & 0x1f;
                }
            }
            0xf000 => self.irq.reload = (self.irq.reload & 0xf0) | (val & 0x0f),
            0xf001 => self.irq.reload = (self.irq.reload & 0x0f) | ((val & 0x0f) << 4),
            0xf002 => self.irq.set_control(val),
            0xf003 => self.irq.acknowledge(),
            _ => {}
        }
        self.sync();
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

    fn ticks_cpu(&self) -> bool {
        true
    }

    fn tick_cpu(&mut self) {
        // Only VRC4 has this counter, but running it when the variant is a
        // guess is harmless: unwritten control registers never fire.
        if self.variant.is_vrc4() || (self.heuristics && self.variant != Variant::Vrc2a) {
            self.irq.tick();
        }
    }

    fn irq(&self) -> bool {
        self.irq.flag
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}
