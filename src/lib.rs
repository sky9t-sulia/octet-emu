//! A cycle-accurate NES emulation core.
//!
//! A closed box shaped like the console: cartridge and controller state in,
//! framebuffer and audio out. No I/O, no knowledge of the host, and no
//! allocation after load.
//!
//! ```no_run
//! use octet_emu::{Cartridge, Nes, Button};
//! # fn read(_: &str) -> Vec<u8> { Vec::new() }
//! let cart = Cartridge::from_ines(&read("game.nes")).unwrap();
//! let mut nes = Nes::new(cart).unwrap();
//! nes.set_button(0, Button::Start, true);
//! nes.run_frame();
//! let pixels = nes.framebuffer(); // 256 x 240, packed 0x00RRGGBB
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::boxed::Box;

pub mod apu;
pub mod cartridge;
pub mod cpu;
pub mod ffi;
pub mod input;
pub mod mapper;
pub mod ppu;
#[cfg(feature = "snapshot")]
mod snapshot;

pub use cartridge::{Cartridge, Mirroring, RomError, TvSystem};
pub use input::Button;
pub use mapper::UnsupportedMapper;
pub use ppu::{FRAME_HEIGHT, FRAME_WIDTH};

use apu::Apu;
use cpu::{Cpu, CpuBus};
use input::Controller;
use mapper::Mapper;
use ppu::Ppu;

#[cfg(feature = "snapshot")]
/// A cartridge's fingerprint: FNV-1a over the ROM, which never changes while
/// the game is loaded and differs between games.
fn fingerprint(cart: &Cartridge) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in cart.prg_rom.iter().chain(cart.chr_rom.iter()) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// What a save state begins with, so that anything else is refused.
#[cfg(feature = "snapshot")]
const SNAPSHOT_MAGIC: [u8; 4] = *b"OCTS";

/// Raised whenever the shape of the saved state changes, which makes every
/// older file refuse to load rather than restore into the wrong fields.
#[cfg(feature = "snapshot")]
const SNAPSHOT_VERSION: u8 = 1;

/// Why a save state was not loaded.
#[cfg(feature = "snapshot")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapshotError {
    NotASnapshot,
    WrongVersion,
    WrongCartridge,
    Corrupt,
}

/// CPU registers at a point in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuState {
    pub pc: u16,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub p: u8,
}

#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub struct Nes {
    cpu: Cpu,
    bus: Bus,
}

/// Everything the CPU can reach. Split from [`Nes`] so the CPU can borrow it
/// mutably for a whole instruction.
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct Bus {
    /// Which region's timings the cartridge asked for.
    tv: TvSystem,
    /// Processor cycles since the last extra dot, for the region that runs
    /// sixteen dots to five cycles rather than three to one.
    dot_debt: u8,
    #[cfg_attr(feature = "snapshot", serde(with = "crate::snapshot::bytes"))]
    ram: [u8; 0x800],
    ppu: Ppu,
    apu: Apu,
    #[cfg_attr(feature = "snapshot", serde(with = "crate::snapshot::mapper"))]
    mapper: Box<dyn Mapper>,
    controllers: [Controller; 2],

    /// Last value on the data bus, returned by reads that decode to nothing.
    open_bus: u8,

    // ---- DMA unit: one machine for both. Reads on get cycles, writes on put.
    /// A DMA has been requested and the CPU must be halted on its next read.
    need_halt: bool,
    /// The DMC's transfer includes a dummy cycle before the real read.
    need_dummy_read: bool,
    /// Set by a write to $4014; raises `need_halt` on the following cycle.
    sprite_dma_delay: bool,
    /// The port last read through $4016/$4017, when, and what it gave.
    prev_input_addr: u16,
    prev_input_cycle: [u64; 2],
    prev_input_value: [u8; 2],
    dmc_dma_running: bool,
    sprite_dma_running: bool,
    sprite_dma_page: u8,
    /// Set once the halt cycle is spent and the transfer proper is under way.
    dma_active: bool,
    /// Progress through the 256 read/write pairs of an OAM transfer.
    sprite_counter: u16,
    sprite_read_addr: u8,
    /// Byte read on a get cycle, written to $2004 on the following put cycle.
    dma_latch: u8,

    /// CPU cycles since power-on. Its parity decides get versus put cycles.
    cycle_count: u64,

    /// A cheap fingerprint of the cartridge, so a save state cannot be
    /// restored into a different game. Named in the file's header, not in its
    /// body, so it can be checked before anything is decoded.
    #[cfg(feature = "snapshot")]
    #[cfg_attr(feature = "snapshot", serde(skip))]
    cart_id: u64,
}

impl Nes {
    pub fn new(cart: Cartridge) -> Result<Self, UnsupportedMapper> {
        let tv = cart.tv;
        #[cfg(feature = "snapshot")]
        let cart_id = fingerprint(&cart);
        let mapper = mapper::create(cart)?;
        let mut nes = Nes {
            cpu: Cpu::new(),
            bus: Bus {
                tv,
                dot_debt: 0,
                ram: [0; 0x800],
                ppu: Ppu::new(tv),
                apu: Apu::new(tv),
                mapper,
                controllers: [Controller::new(), Controller::new()],
                open_bus: 0,
                need_halt: false,
                need_dummy_read: false,
                sprite_dma_delay: false,
                prev_input_addr: 0,
                prev_input_cycle: [u64::MAX; 2],
                prev_input_value: [0; 2],
                dmc_dma_running: false,
                sprite_dma_running: false,
                sprite_dma_page: 0,
                dma_active: false,
                sprite_counter: 0,
                sprite_read_addr: 0,
                dma_latch: 0,
                // Incremented at the start of a cycle, so the first is 0 and
                // get/put parity lines up with the hardware's.
                cycle_count: u64::MAX,
                #[cfg(feature = "snapshot")]
                cart_id,
            },
        };
        nes.reset();
        Ok(nes)
    }

    /// Pulse the reset line. RAM and most PPU state survive, as on hardware.
    pub fn reset(&mut self) {
        self.bus.ppu.reset();
        self.bus.apu.reset();
        self.cpu.reset(&mut self.bus);
    }

    /// Run until the PPU completes the frame it is currently drawing.
    pub fn run_frame(&mut self) {
        let target = self.bus.ppu.frame;
        while self.bus.ppu.frame == target {
            self.cpu.step(&mut self.bus);
        }
    }

    /// Advance by a single instruction, for debuggers and steppers.
    pub fn step_instruction(&mut self) {
        self.cpu.step(&mut self.bus);
    }

    /// The completed frame: 256 x 240 pixels, each `0x00RRGGBB`.
    pub fn framebuffer(&self) -> &[u32; FRAME_WIDTH * FRAME_HEIGHT] {
        self.bus.ppu.framebuffer()
    }

    /// Drain the audio produced since the last call, as mono samples in
    /// `-1.0..=1.0` at the rate reported by [`Nes::sample_rate`].
    pub fn drain_audio(&mut self, out: &mut alloc::vec::Vec<f32>) {
        self.bus.apu.drain(out);
    }

    pub fn sample_rate(&self) -> u32 {
        self.bus.apu.sample_rate()
    }

    /// How many pictures the console draws a second, in thousandths. A
    /// cartridge for one region run at the other's rate is the wrong speed by
    /// a fifth, which is heard long before it is seen.
    pub fn frame_rate(&self) -> u32 {
        match self.bus.tv {
            TvSystem::Ntsc => 60_098,
            TvSystem::Pal => 50_007,
            TvSystem::Dendy => 50_007,
        }
    }

    pub fn set_button(&mut self, port: usize, button: Button, pressed: bool) {
        self.bus.controllers[port].set(button, pressed);
    }

    /// Battery-backed cartridge RAM, to be written to a save file.
    pub fn save_data(&self) -> &[u8] {
        self.bus.mapper.nvram()
    }

    pub fn load_save_data(&mut self, data: &[u8]) {
        let dst = self.bus.mapper.nvram_mut();
        let n = dst.len().min(data.len());
        dst[..n].copy_from_slice(&data[..n]);
    }

    /// Total CPU cycles since power-on.
    pub fn cycles(&self) -> u64 {
        self.cpu.cycles
    }

    /// The whole machine as bytes, to be given back to [`Nes::load_state`].
    ///
    /// The cartridge's ROM is not in it: that is on the cartridge, and a save
    /// state is only what changed since it was plugged in.
    #[cfg(feature = "snapshot")]
    pub fn save_state(&self) -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec::Vec::new();
        out.extend_from_slice(&SNAPSHOT_MAGIC);
        out.push(SNAPSHOT_VERSION);
        out.extend_from_slice(&self.bus.cart_id.to_le_bytes());
        if let Ok(body) = postcard::to_allocvec(self) {
            out.extend_from_slice(&body);
        }
        out
    }

    /// Put the machine back where a save state left it.
    ///
    /// Refuses bytes that are not a save state, were written by an older
    /// build, or came from a different cartridge -- each of which would
    /// otherwise restore into a machine that quietly makes no sense.
    #[cfg(feature = "snapshot")]
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let head = SNAPSHOT_MAGIC.len() + 1 + 8;
        if data.len() < head || data[..SNAPSHOT_MAGIC.len()] != SNAPSHOT_MAGIC {
            return Err(SnapshotError::NotASnapshot);
        }
        if data[SNAPSHOT_MAGIC.len()] != SNAPSHOT_VERSION {
            return Err(SnapshotError::WrongVersion);
        }
        let mut id = [0u8; 8];
        id.copy_from_slice(&data[SNAPSHOT_MAGIC.len() + 1..head]);
        if u64::from_le_bytes(id) != self.bus.cart_id {
            return Err(SnapshotError::WrongCartridge);
        }

        let mut loaded: Nes =
            postcard::from_bytes(&data[head..]).map_err(|_| SnapshotError::Corrupt)?;

        // What came out of the file carries only the mapper's registers. The
        // board itself -- with the ROM on it -- is the one already here.
        let pending = loaded.bus.mapper.save_state();
        if !self.bus.mapper.load_state(&pending) {
            return Err(SnapshotError::Corrupt);
        }
        let board = core::mem::replace(
            &mut self.bus.mapper,
            alloc::boxed::Box::new(snapshot::Pending(alloc::vec::Vec::new())),
        );
        loaded.bus.mapper = board;
        // The fingerprint is named in the header rather than the body, so it
        // is not in what was read back: carry the running one over, or the
        // next state saved would claim to belong to no cartridge at all.
        loaded.bus.cart_id = self.bus.cart_id;
        *self = loaded;
        Ok(())
    }
}

/// Which of the three PPU dots in a CPU cycle the bus access is aligned to.
const ACCESS_DOT: u32 = 2;

impl Bus {
    /// Account for one DMA cycle passing: the halt cycle is spent first, then
    /// the DMC's dummy cycle, before any real transfer can happen.
    fn consume_dma_cycle(&mut self) {
        if self.need_halt {
            self.need_halt = false;
        } else if self.need_dummy_read {
            self.need_dummy_read = false;
        }
    }

    /// Read a controller port. The strobe clocks the shift register, not the
    /// read, so consecutive reads of one port advance it only once.
    fn read_controller(&mut self, port: usize) -> u8 {
        let addr = 0x4016 + port as u16;
        let strobe_held = self.prev_input_addr == addr
            && self.cycle_count.wrapping_sub(self.prev_input_cycle[port]) <= 1;
        if !strobe_held {
            self.prev_input_value[port] = self.controllers[port].read();
        }
        self.prev_input_addr = addr;
        self.prev_input_cycle[port] = self.cycle_count;
        self.prev_input_value[port]
    }

    /// A DMA read. If the CPU was halted reading `halted_at` in $4000-$401F,
    /// the 2A03 reads that register too, which is the DMC input glitch.
    fn dma_read(&mut self, addr: u16, halted_at: u16) -> u8 {
        self.tick_before();
        let val = self.dma_read_raw(addr, halted_at);
        self.tick_after();
        val
    }

    fn dma_read_raw(&mut self, addr: u16, halted_at: u16) -> u8 {
        if halted_at & 0xffe0 != 0x4000 {
            // Outside that window the DMA simply reads the bus, except that
            // the 2A03's readable registers are not visible to it at all.
            if (0x4015..=0x401a).contains(&addr) {
                return self.open_bus;
            }
            return self.read_raw(addr);
        }

        let internal = 0x4000 | (addr & 0x1f);
        if internal == addr {
            return self.read_raw(addr);
        }
        match internal {
            // Reading $4015 drives only the CPU's internal bus, so the read the
            // DMA meant to do still happens on the external one.
            0x4015 => {
                let external_bus = self.open_bus;
                let val = self.read_raw(0x4015);
                self.open_bus = external_bus;
                self.read_raw(addr);
                val
            }
            0x4016 | 0x4017 => {
                let val = self.read_raw(internal);
                let external = self.read_raw(addr);
                // The joypad holds the bus longer than the cartridge, so it
                // wins its bits; the rest collide and read as an AND.
                const OPEN: u8 = 0xe0;
                self.open_bus = (external & OPEN) | (val & !OPEN);
                (external & OPEN) | (val & external & !OPEN)
            }
            _ => self.read_raw(addr),
        }
    }

    /// Dots that elapse before the CPU's access takes effect.
    fn tick_before(&mut self) {
        for _ in 0..ACCESS_DOT {
            self.ppu.tick_dot(&mut *self.mapper);
        }
        // APU and mapper run at the start of the cycle, the PPU straddles it.
        // This phase decides when APU interrupts land.
        self.cycle_count = self.cycle_count.wrapping_add(1);
        self.apu.tick();
        // The DMC asks for a byte through the same halt mechanism OAM DMA
        // uses, and the two can be in flight at once.
        if self.apu.take_dmc_request() {
            self.dmc_dma_running = true;
            self.need_dummy_read = true;
            self.need_halt = true;
        }
        if core::mem::take(&mut self.sprite_dma_delay) {
            self.need_halt = true;
        }
        self.mapper.tick_cpu();
    }

    /// Dots that elapse after it, completing the cycle.
    fn tick_after(&mut self) {
        for _ in ACCESS_DOT..3 {
            self.ppu.tick_dot(&mut *self.mapper);
        }
        // One region clocks its picture at three and a fifth dots to the
        // processor's cycle rather than three, so every fifth cycle owes one
        // more. Without it a frame there runs six and a half per cent long,
        // which is a game that plays slow and sound that arrives faster than
        // it can be heard.
        if self.tv == TvSystem::Pal {
            self.dot_debt += 1;
            if self.dot_debt == 5 {
                self.dot_debt = 0;
                self.ppu.tick_dot(&mut *self.mapper);
            }
        }
    }
}

impl Bus {
    /// The bus access itself, with no cycle attached. A DMA cycle can perform
    /// more than one of these, so the ticking lives in the callers.
    fn read_raw(&mut self, addr: u16) -> u8 {
        let val = match addr {
            0x0000..=0x1fff => self.ram[addr as usize & 0x07ff],
            0x2000..=0x3fff => self.ppu.read_register(addr, &mut *self.mapper, self.open_bus),
            0x4015 => self.apu.read_status(),
            0x4016 => (self.open_bus & 0xe0) | self.read_controller(0),
            0x4017 => (self.open_bus & 0xe0) | self.read_controller(1),
            // The rest of the $4000 block is write-only and reads as open bus.
            0x4000..=0x401f => self.open_bus,
            _ => self.mapper.cpu_read(addr).unwrap_or(self.open_bus),
        };
        self.open_bus = val;
        val
    }

    fn write_raw(&mut self, addr: u16, val: u8) {
        self.open_bus = val;
        match addr {
            0x0000..=0x1fff => self.ram[addr as usize & 0x07ff] = val,
            0x2000..=0x3fff => self.ppu.write_register(addr, val, &mut *self.mapper),
            0x4014 => {
                self.sprite_dma_running = true;
                self.sprite_dma_page = val;
                // The DMC's request is raised at the start of a cycle, this
                // write at the end, so its halt begins a cycle later.
                self.sprite_dma_delay = true;
            }
            0x4016 => {
                self.controllers[0].strobe(val);
                self.controllers[1].strobe(val);
            }
            0x4000..=0x4017 => self.apu.write_register(addr, val),
            0x4018..=0x401f => {}
            _ => self.mapper.cpu_write(addr, val),
        }
    }
}

impl CpuBus for Bus {
    fn read(&mut self, addr: u16) -> u8 {
        self.tick_before();
        let val = self.read_raw(addr);
        self.tick_after();
        val
    }

    fn write(&mut self, addr: u16, val: u8) {
        self.tick_before();
        self.write_raw(addr, val);
        self.tick_after();
    }

    fn nmi(&self) -> bool {
        self.ppu.nmi_line()
    }

    fn irq(&self) -> bool {
        self.apu.irq() || self.mapper.irq()
    }

    fn dma_pending(&self) -> bool {
        self.need_halt || (self.dma_active && (self.dmc_dma_running || self.sprite_dma_running))
    }

    /// Advance the DMA by one cycle, so the CPU keeps polling its interrupt
    /// lines throughout. `read_addr` is what it was about to read.
    fn dma_step(&mut self, read_addr: u16) {
        if !self.dma_active {
            // The halt cycle: the CPU's read is repeated and discarded.
            self.dma_active = true;
            self.need_halt = false;
            self.sprite_counter = 0;
            self.sprite_read_addr = 0;
            self.dma_read(read_addr, read_addr);
            return;
        }

        // Reads are only possible on get cycles and writes on put cycles;
        // anything else is an alignment read.
        let get_cycle = self.cycle_count % 2 == 0;
        if get_cycle {
            if self.dmc_dma_running && !self.need_halt && !self.need_dummy_read {
                self.consume_dma_cycle();
                let addr = self.apu.dmc_read_address();
                let val = self.dma_read(addr, read_addr);
                self.dmc_dma_running = false;
                self.apu.load_dmc_sample(val);
            } else if self.sprite_dma_running {
                self.consume_dma_cycle();
                let addr = ((self.sprite_dma_page as u16) << 8) | self.sprite_read_addr as u16;
                self.dma_latch = self.dma_read(addr, read_addr);
                self.sprite_read_addr = self.sprite_read_addr.wrapping_add(1);
                self.sprite_counter += 1;
            } else {
                // The DMC is waiting out its halt or dummy cycle.
                self.consume_dma_cycle();
                self.dma_read(read_addr, read_addr);
            }
        } else if self.sprite_dma_running && self.sprite_counter & 1 != 0 {
            self.consume_dma_cycle();
            let val = self.dma_latch;
            self.write(0x2004, val);
            self.sprite_counter += 1;
            if self.sprite_counter == 0x200 {
                self.sprite_dma_running = false;
            }
        } else {
            // Align to the next get cycle.
            self.consume_dma_cycle();
            self.dma_read(read_addr, read_addr);
        }

        if !self.dmc_dma_running && !self.sprite_dma_running {
            self.dma_active = false;
        }
    }
}

impl Nes {
    /// A snapshot of the CPU registers, for debugging and tracing.
    pub fn cpu_state(&self) -> CpuState {
        CpuState {
            pc: self.cpu.pc,
            a: self.cpu.a,
            x: self.cpu.x,
            y: self.cpu.y,
            s: self.cpu.s,
            p: self.cpu.p,
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Build a minimal NROM cartridge running `code` from $C000.
    fn cart_with(code: &[u8]) -> Cartridge {
        let mut prg = vec![0; 16 * 1024];
        prg[..code.len()].copy_from_slice(code);
        // Reset vector at $FFFC points at $C000, the start of this bank.
        prg[0x3ffc] = 0x00;
        prg[0x3ffd] = 0xc0;
        Cartridge {
            prg_rom: prg,
            chr_rom: Vec::new(),
            chr_ram: vec![0; 8192],
            prg_ram: vec![0; 8192],
            prg_nvram: Vec::new(),
            mapper: 0,
            submapper: 0,
            mirroring: Mirroring::Horizontal,
            tv: TvSystem::Ntsc,
        }
    }

    /// A save state must restore the machine to the state it was taken from,
    /// not merely to some state.
    #[cfg(feature = "snapshot")]
    #[test]
    fn a_save_state_puts_the_machine_back_where_it_was() {
        let code = [0xa9, 0x11, 0x8d, 0x00, 0x20, 0x69, 0x01, 0x8d, 0x00, 0x20, 0x00];
        let mut nes = Nes::new(cart_with(&code)).unwrap();

        nes.set_button(0, Button::Start, true);
        nes.run_frame();
        let saved = nes.save_state();
        let was = nes.cpu_state();
        let then = nes.cycles();

        nes.set_button(0, Button::Start, false);
        for _ in 0..10 {
            nes.run_frame();
        }
        // Without this the rest would hold for a load that restored nothing,
        // since the machine would never have left where it was.
        assert_ne!(nes.cycles(), then, "ten frames changed nothing");

        nes.load_state(&saved).unwrap();
        assert_eq!(nes.cpu_state(), was);
        assert_eq!(nes.cycles(), then);
    }

    /// A state saved after loading one must be as good as the first, and the
    /// difference only shows on a fresh console: within one machine a lost
    /// fingerprint is compared against itself and matches.
    #[cfg(feature = "snapshot")]
    #[test]
    fn a_state_saved_after_loading_one_survives_a_restart() {
        let mut nes = Nes::new(cart_with(&[0xea])).unwrap();
        nes.run_frame();
        let first = nes.save_state();

        nes.run_frame();
        nes.load_state(&first).expect("the first state was refused");

        // Carry on from there and save again, as a player would.
        nes.run_frame();
        let second = nes.save_state();

        // Now start the cartridge afresh, which is what closing the app does.
        let mut later = Nes::new(cart_with(&[0xea])).unwrap();
        assert_eq!(
            later.load_state(&second),
            Ok(()),
            "a state saved after a load was refused by the same cartridge",
        );
    }

    /// A save state carries the machine, not the cartridge: a bigger ROM must
    /// not make a bigger file, because the ROM is not in it at all.
    #[cfg(feature = "snapshot")]
    #[test]
    fn a_save_state_leaves_the_rom_behind() {
        fn saved_size(banks: usize) -> usize {
            let mut cart = cart_with(&[0xea]);
            // NROM reads the last 16 KiB, where the reset vector already is.
            let mut big = vec![0; 16 * 1024 * (banks - 1)];
            big.extend_from_slice(&cart.prg_rom);
            cart.prg_rom = big;
            let mut nes = Nes::new(cart).unwrap();
            nes.run_frame();
            nes.save_state().len()
        }

        // Not exactly equal: the window offsets are written as varints, and
        // a larger ROM needs a byte or two more to say where a window points.
        let grew = saved_size(16) - saved_size(1);
        assert!(grew < 64, "a sixteenfold rom grew the file by {grew} bytes");
    }

    /// Anything that is not this build's save state for this cartridge is
    /// refused, rather than restored into a machine that makes no sense.
    #[cfg(feature = "snapshot")]
    #[test]
    fn a_save_state_from_elsewhere_is_refused() {
        let mut nes = Nes::new(cart_with(&[0xea])).unwrap();
        nes.run_frame();
        let saved = nes.save_state();

        assert_eq!(nes.load_state(b"not a save state at all"), Err(SnapshotError::NotASnapshot));

        let mut older = saved.clone();
        older[SNAPSHOT_MAGIC.len()] = SNAPSHOT_VERSION.wrapping_add(1);
        assert_eq!(nes.load_state(&older), Err(SnapshotError::WrongVersion));

        let mut other = Nes::new(cart_with(&[0xa9, 0x42])).unwrap();
        assert_eq!(other.load_state(&saved), Err(SnapshotError::WrongCartridge));
    }

    /// An OAM transfer costs 513 cycles, or 514 when it must wait a cycle to
    /// align with the read half of the bus.
    #[test]
    fn oam_dma_costs_513_or_514_cycles() {
        // LDA #$02 / STA $4014 / NOP / JMP *
        let code = [0xa9, 0x02, 0x8d, 0x14, 0x40, 0xea, 0x4c, 0x05, 0xc0];
        let mut nes = Nes::new(cart_with(&code)).unwrap();

        nes.step_instruction(); // LDA
        nes.step_instruction(); // STA $4014 — queues the transfer
        let before = nes.cycles();
        // The DMA halts the CPU on the next read, which is this opcode fetch.
        nes.step_instruction(); // NOP
        let cost = nes.cycles() - before;

        // Two cycles for the NOP itself, plus the transfer.
        let dma = cost - 2;
        assert!(dma == 513 || dma == 514, "OAM DMA took {dma} cycles, expected 513 or 514");
    }

    /// Shifting the write by an odd number of cycles must swap the alignment.
    /// Equal costs would mean the alignment cycle is never applied.
    #[test]
    fn oam_dma_alignment_alternates_with_the_write_cycle() {
        // `pad` is one instruction of the given length in cycles.
        let run = |pad: &[u8], pad_instructions: usize| {
            let mut code = pad.to_vec();
            code.extend_from_slice(&[0xa9, 0x02, 0x8d, 0x14, 0x40, 0xea]);
            let mut nes = Nes::new(cart_with(&code)).unwrap();
            for _ in 0..pad_instructions {
                nes.step_instruction();
            }
            nes.step_instruction(); // LDA #$02
            nes.step_instruction(); // STA $4014
            let before = nes.cycles();
            nes.step_instruction(); // NOP, during whose fetch the DMA runs
            nes.cycles() - before - 2
        };

        let even = run(&[], 0);
        // STA $00 is three cycles, so it flips the parity of the $4014 write.
        let odd = run(&[0x85, 0x00], 1);
        assert!(even == 513 || even == 514, "DMA cost {even}");
        assert!(odd == 513 || odd == 514, "DMA cost {odd}");
        assert_ne!(even, odd, "an odd cycle shift must change the alignment, but both cost {even}");
    }
}
