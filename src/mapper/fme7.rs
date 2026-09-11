//! Sunsoft FME-7, 5A and 5B — mapper 69.
//!
//! One chip behind three names; the 5A and 5B add a sound chip, whose
//! registers are ignored here, so those games play without part of the music.
//!
//! Sixteen registers reached through two ports: the number to `$8000`, the
//! value to `$A000`.

use super::{Banks, Mapper};
use crate::cartridge::{Cartridge, Mirroring};

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct Fme7 {
    banks: Banks,
    /// Which register the next write to `$A000` lands in.
    command: u8,
    mirroring: Mirroring,
    /// The last value written to command 8, kept so that a later change of
    /// PRG RAM size or battery does not have to guess what the game asked for.
    wram_control: u8,

    /// Counts down once per CPU cycle while enabled, and asks for an interrupt
    /// on the cycle it passes zero.
    irq_counter: u16,
    irq_counting: bool,
    irq_enabled: bool,
    irq_line: bool,
}

impl Fme7 {
    pub fn new(cart: Cartridge) -> Self {
        let mirroring = cart.mirroring;
        let mut fme7 = Fme7 {
            banks: Banks::new(cart),
            command: 0,
            mirroring,
            wram_control: 0,
            irq_counter: 0,
            irq_counting: false,
            irq_enabled: false,
            irq_line: false,
        };
        // The window at $E000 is soldered to the last bank and never moves;
        // the other three start wherever, since a game sets them before it
        // reads anything through them.
        fme7.banks.map_prg_8k(3, -1);
        fme7
    }

    /// Carry out the command in `self.command` with `value` as its parameter.
    fn apply(&mut self, value: u8) {
        match self.command & 0x0f {
            // A whole byte of bank each: 256 KiB of characters.
            slot @ 0x0..=0x7 => self.banks.map_chr_1k(slot as usize, value as isize),

            // Bit 6: RAM or program. Bit 7, for RAM only: reachable or not.
            0x8 => {
                self.wram_control = value;
                let bank = (value & 0x3f) as isize;
                if value & 0x40 != 0 {
                    if value & 0x80 != 0 {
                        self.banks.map_wram(Some(bank), false);
                    } else {
                        self.banks.map_wram(None, false);
                    }
                } else {
                    // ROM in a window the rest of the machine expects to be
                    // RAM. Banks has no call for this, so the slot is mapped
                    // as ROM and reads are answered in cpu_peek below.
                    self.banks.map_wram(None, false);
                }
            }

            // Three 8 KiB program windows. The fourth is fixed.
            slot @ 0x9..=0xb => {
                self.banks.map_prg_8k(slot as usize - 9, (value & 0x3f) as isize)
            }

            // Zero is vertical here, where most mappers make zero horizontal.
            // For some reason the wiki reads the other way round.
            0xc => {
                self.mirroring = match value & 0x03 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenA,
                    _ => Mirroring::SingleScreenB,
                }
            }

            // Counting and interrupting are separate switches. Any write
            // here acknowledges a pending interrupt -- the only way to.
            0xd => {
                self.irq_counting = value & 0x80 != 0;
                self.irq_enabled = value & 0x01 != 0;
                self.irq_line = false;
            }

            // Written directly, not into a reload latch; each half alone.
            0xe => self.irq_counter = (self.irq_counter & 0xff00) | value as u16,
            _ => self.irq_counter = (self.irq_counter & 0x00ff) | ((value as u16) << 8),
        }
    }

    /// Whether `$6000` is showing a bank of the program rather than RAM.
    fn wram_is_rom(&self) -> bool {
        self.wram_control & 0x40 == 0
    }
}

impl Mapper for Fme7 {
    #[cfg(feature = "snapshot")]
    super::state_via_serde!();

    fn cpu_peek(&self, addr: u16) -> Option<u8> {
        if (0x6000..=0x7fff).contains(&addr) && self.wram_is_rom() {
            let bank = (self.wram_control & 0x3f) as usize;
            let count = (self.banks.prg_rom.len() / 8192).max(1);
            let at = (bank % count) * 8192 + (addr as usize & 0x1fff);
            return self.banks.prg_rom.get(at).copied();
        }
        self.banks.read_cpu(addr)
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr & 0xe000 {
            0x8000 => self.command = val & 0x0f,
            0xa000 => self.apply(val),
            // $C000 and $E000 are the sound chip's own command and parameter
            // ports on a 5A or 5B, and nothing at all on an FME-7.
            _ => {
                self.banks.write_cpu(addr, val);
            }
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
        self.irq_line
    }

    fn ticks_cpu(&self) -> bool {
        true
    }

    fn tick_cpu(&mut self) {
        if !self.irq_counting {
            return;
        }
        // Keeps going from $FFFF rather than stopping, so a counter left
        // running interrupts every 65536 cycles.
        self.irq_counter = self.irq_counter.wrapping_sub(1);
        if self.irq_counter == 0xffff && self.irq_enabled {
            self.irq_line = true;
        }
    }

    fn nvram(&self) -> &[u8] {
        self.banks.nvram()
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        self.banks.nvram_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::TvSystem;
    use alloc::vec;

    /// A board with enough of each memory to tell one bank from another.
    fn board() -> Fme7 {
        let mut prg_rom = vec![0; 64 * 1024];
        // Stamp each 8 KiB bank with its own number, so a read says which
        // bank answered it.
        for (bank, chunk) in prg_rom.chunks_mut(8192).enumerate() {
            chunk.fill(bank as u8);
        }
        Fme7::new(Cartridge {
            prg_rom,
            chr_rom: vec![0; 8192],
            chr_ram: vec![],
            prg_ram: vec![0; 8192],
            prg_nvram: vec![],
            mapper: 69,
            submapper: 0,
            mirroring: Mirroring::Vertical,
            tv: TvSystem::Ntsc,
        })
    }

    /// Write `value` into register `command`, the long way round: this chip
    /// has no address per register, and getting the two ports the wrong way
    /// about is the mistake worth catching.
    fn poke(fme7: &mut Fme7, command: u8, value: u8) {
        fme7.cpu_write(0x8000, command);
        fme7.cpu_write(0xa000, value);
    }

    #[test]
    fn the_last_bank_stays_where_it_is() {
        let mut fme7 = board();
        assert_eq!(fme7.cpu_peek(0xe000), Some(7), "the fixed window moved");
        // Every other window is the game's to place, and placing one must not
        // disturb the fixed one.
        poke(&mut fme7, 0x9, 3);
        poke(&mut fme7, 0xa, 4);
        poke(&mut fme7, 0xb, 5);
        assert_eq!(fme7.cpu_peek(0x8000), Some(3));
        assert_eq!(fme7.cpu_peek(0xa000), Some(4));
        assert_eq!(fme7.cpu_peek(0xc000), Some(5));
        assert_eq!(fme7.cpu_peek(0xe000), Some(7));
    }

    #[test]
    fn the_window_at_6000_is_ram_or_program_as_asked() {
        let mut fme7 = board();
        // Bit 6 for RAM, bit 7 to let the CPU reach it.
        poke(&mut fme7, 0x8, 0xc0);
        fme7.cpu_write(0x6000, 0x5a);
        assert_eq!(fme7.cpu_peek(0x6000), Some(0x5a), "RAM did not hold a write");

        // Bit 6 clear: the window shows a bank of the program instead.
        poke(&mut fme7, 0x8, 0x02);
        assert_eq!(fme7.cpu_peek(0x6000), Some(2), "the program is not visible");

        // RAM again, but closed. What comes back is not the RAM.
        poke(&mut fme7, 0x8, 0x40);
        assert_ne!(fme7.cpu_peek(0x6000), Some(0x5a), "a closed window answered");
    }

    /// Zero is vertical on this chip, where most mappers make zero horizontal.
    #[test]
    fn mirroring_follows_sunsofts_numbering() {
        let mut fme7 = board();
        for (value, want) in [
            (0, Mirroring::Vertical),
            (1, Mirroring::Horizontal),
            (2, Mirroring::SingleScreenA),
            (3, Mirroring::SingleScreenB),
        ] {
            poke(&mut fme7, 0xc, value);
            assert_eq!(fme7.mirroring(), want, "mirroring {value}");
        }
    }

    #[test]
    fn the_counter_interrupts_on_the_cycle_it_passes_zero() {
        let mut fme7 = board();
        poke(&mut fme7, 0xe, 3); // low byte
        poke(&mut fme7, 0xf, 0); // high byte
        poke(&mut fme7, 0xd, 0x81); // counting, and allowed to interrupt

        for cycle in 0..3 {
            fme7.tick_cpu();
            assert!(!fme7.irq(), "interrupted early, on cycle {cycle}");
        }
        fme7.tick_cpu();
        assert!(fme7.irq(), "no interrupt when the counter passed zero");

        // Any write to the control register acknowledges it; there is no
        // other way for a handler to clear the line.
        poke(&mut fme7, 0xd, 0x81);
        assert!(!fme7.irq(), "the interrupt outlived its acknowledgement");
    }

    #[test]
    fn a_stopped_counter_does_not_count_and_a_muted_one_does_not_interrupt() {
        let mut fme7 = board();
        poke(&mut fme7, 0xe, 1);
        poke(&mut fme7, 0xd, 0x01); // may interrupt, but is not running
        for _ in 0..10 {
            fme7.tick_cpu();
        }
        assert!(!fme7.irq(), "a stopped counter counted");

        // Running but not allowed to interrupt: it still counts, and passing
        // zero costs nothing.
        poke(&mut fme7, 0xe, 1);
        poke(&mut fme7, 0xf, 0);
        poke(&mut fme7, 0xd, 0x80);
        for _ in 0..10 {
            fme7.tick_cpu();
        }
        assert!(!fme7.irq(), "a muted counter interrupted");
    }
}
