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
//! let frame = nes.color_ids(); // one identity a pixel; expand with `palette_table`
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
pub use ppu::{expand, palette_table, COLOR_IDS, FRAME_HEIGHT, FRAME_WIDTH};

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

    /// Last value on the external data bus, returned by reads that decode to
    /// nothing.
    open_bus: u8,
    /// Last value on the processor's own data bus.
    ///
    /// Two buses, not one. Every access puts its value on this one, a `$4015`
    /// read included — and that read is the one access that never reaches the
    /// external bus. A transfer's reads are the mirror case: they drive the
    /// external bus and leave this alone, because the processor is not the one
    /// doing them.
    ///
    /// Bit 5 of `$4015` has no flag behind it and reports this.
    internal_bus: u8,

    /// Whether the cartridge wants a call every processor cycle. Asked once,
    /// when it is opened; four mappers of fifteen say yes.
    mapper_ticks_cpu: bool,

    // ---- DMA unit. Two requesters, one bus: see `dma_step`.
    /// The sprite transfer and the DMC's fetch, each at whatever cycle it is
    /// up to. Both can be in flight at once, which is the whole difficulty.
    oam: Oam,
    dmc: Dmc,
    /// The page `$4014` named.
    sprite_dma_page: u8,
    /// Byte read on a get cycle, written to $2004 on the following put cycle.
    dma_latch: u8,
    /// A 'write' to `$4016` waiting for the phase that acts on it.
    ///
    /// The strobe does not land where the 'write' happens: it waits for the edge
    /// out of a get cycle, so a 'write' on the wrong phase takes a cycle longer
    /// to reach the pads than one on the right phase.
    strobe_pending: Option<u8>,
    /// The port last read through $4016/$4017, when, and what it gave.
    prev_input_addr: u16,
    prev_input_cycle: [u64; 2],
    prev_input_value: [u8; 2],

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
        let watches_bus = mapper.watches_ppu_bus();
        let serves_nametables = mapper.serves_nametables();
        let ticks_cpu = mapper.ticks_cpu();
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
                internal_bus: 0,
                mapper_ticks_cpu: false,
                oam: Oam::Idle,
                dmc: Dmc::Idle,
                strobe_pending: None,
                prev_input_addr: 0,
                prev_input_cycle: [u64::MAX; 2],
                prev_input_value: [0; 2],
                sprite_dma_page: 0,
                dma_latch: 0,
                // Incremented at the start of a cycle, so the first is 0 and
                // get/put parity lines up with the hardware's.
                cycle_count: u64::MAX,
                #[cfg(feature = "snapshot")]
                cart_id,
            },
        };
        nes.bus.ppu.set_mapper_interest(watches_bus, serves_nametables);
        let mirroring = nes.bus.mapper.mirroring();
        nes.bus.ppu.set_mirroring(mirroring);
        nes.bus.mapper_ticks_cpu = ticks_cpu;
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

    /// The completed frame as the PPU's own color identities: the six-bit
    /// palette entry in the low bits, the three emphasis bits above it.
    ///
    /// This is the picture, and the only form of it: what the chip put out,
    /// before anyone decided what colour that is. Expand it with
    /// [`palette_table`], or rebuild the signal from it and decode that.
    pub fn color_ids(&self) -> &[u16; FRAME_WIDTH * FRAME_HEIGHT] {
        self.bus.ppu.color_ids()
    }

    /// Drain the audio produced since the last call, as mono samples in
    /// `-1.0...=1.0` at the rate reported by [`Nes::sample_rate`].
    pub fn drain_audio(&mut self, out: &mut Vec<f32>) {
        self.bus.apu.drain(out);
    }

    /// Drain the audio produced since the last call as interleaved stereo
    /// pairs, left then right, the right delayed a few milliseconds.
    ///
    /// The console is mono, so the second channel is made rather than
    /// emulated: see [`apu::STEREO_DELAY_MS`]. [`Nes::drain_audio`] still
    /// gives the one channel the hardware actually produced.
    pub fn drain_audio_stereo(&mut self, out: &mut Vec<f32>) {
        self.bus.apu.drain_stereo(out);
    }

    /// How far the second channel lags, in milliseconds. Zero for mono.
    pub fn set_stereo_delay(&mut self, ms: f32) {
        self.bus.apu.set_stereo_delay(ms);
    }

    /// The delay currently in use, in milliseconds.
    pub fn stereo_delay(&self) -> f32 {
        self.bus.apu.stereo_delay()
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
    pub fn save_state(&self) -> Vec<u8> {
        let mut out = Vec::new();
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
            Box::new(snapshot::Pending(Vec::new())),
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

/// Which half of the processor's clock a cycle falls on.
///
/// The 2A03 divides the processor clock in two, and which half a cycle is on
/// decides what may happen on it. A transfer may read on a get and write on a
/// put, and several things in the APU happen on one of the two *edges*
/// between them rather than during a cycle at all. Those edges are what the
/// hardware documentation talks about, so they are what this models — a bare
/// parity invites each caller to decide for itself which way round it meant,
/// and three of them once got it backwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub enum Phase {
    /// A cycle a transfer may read on.
    Get,
    /// A cycle a transfer may write on.
    Put,
}

/// Which parity of [`Bus::cycle_count`] is a get cycle.
///
/// The console alternates between cycles a transfer may read on and cycles it
/// may write on. Which one a given CPU cycle is carries no meaning by itself —
/// on hardware the phase at power-on is not even fixed — but every decision
/// the DMA unit makes hangs off it, so it is named once, here.
const GET_PHASE: u64 = 1;

/// How far the sprite transfer has got.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
enum Oam {
    Idle,
    /// `$4014` has been written, but the request is not up yet.
    ///
    /// Page 13's `DMC DMA + OAM DMA` wants the halt a cycle earlier than
    /// this, and its answer key matches exactly without this state. But
    /// moving the halt also moves the transfer onto the other phase: it gains
    /// the alignment cycle it did not need, and the whole transfer runs 514
    /// cycles instead of 513. That cycle shifts everything downstream and
    /// costs five other tests. The two want the phase itself corrected, not
    /// the halt moved, and that reaches further than this enum.
    Queued,
    /// Waiting to take the bus off the processor. A 'write' already drives the
    /// bus, so a halt landing on one waits for the processor to read.
    Asked,
    /// The halt is spent. Waiting for a get cycle to start moving on.
    Aligning,
    /// Moving bytes, counted in cycles rather than bytes: five hundred and
    /// twelve steps, the even ones reading and the odd ones writing.
    Moving(u16),
}

/// How far the DMC's fetch has got.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
enum Dmc {
    Idle,
    /// Waiting to take the bus off the processor.
    Asked,
    /// The halt is spent; the dummy cycle is next.
    Dummy,
    /// The dummy is spent. Waiting for a get cycle to read on.
    Aligning,
}

impl Bus {
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
        // The transfer drives the external bus; the processor's own bus is not
        // party to it and keeps whatever the processor last put there.
        let held = self.internal_bus;
        let val = self.read_cycle(addr, |bus| bus.dma_read_raw(addr, halted_at));
        self.internal_bus = held;
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
            // The status read is answered on the internal bus, so the read the
            // DMA meant to do still happens on the external one.
            0x4015 => {
                let val = self.read_raw(0x4015);
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
        // The APU is handed the phase rather than counting it out itself, so
        // that the one divider in here is the one everything reads.
        self.apu.tick(Self::phase(self.cycle_count));
        // The DMC asks for a byte. Its halt and dummy cycles always happen,
        // whether the sprite transfer is already running: see
        // `dma_step` for who gets to move something on them.
        if self.apu.take_dmc_request() {
            self.dmc = Dmc::Asked;
        }
        // Disabling the channel gives up a fetch that is still taking the
        // bus. One that has not spent a cycle yet simply never does; one that
        // has spent its halt stops there, a cycle of stall and nothing else.
        // Past that the fetch is committed and reads regardless — by
        // then it is only waiting for a get cycle to read on.
        if self.apu.take_dmc_abort() && matches!(self.dmc, Dmc::Asked | Dmc::Dummy) {
            self.dmc = Dmc::Idle;
        }
        // "Controllers should be strobed when the CPU transitions from a get
        // cycle to a put cycle", and not on the opposite edge — so it lands
        // at the start of a put, rather than where the 'write' itself did. A
        // 'write' arriving on a get waits for the next put.
        if Self::phase(self.cycle_count) == Phase::Put {
            if let Some(val) = self.strobe_pending.take() {
                self.controllers[0].strobe(val);
                self.controllers[1].strobe(val);
            }
        }
        if self.mapper_ticks_cpu {
            self.mapper.tick_cpu();
        }
    }

    /// Dots that elapse after it, completing the cycle.
    fn tick_after(&mut self) {
        for _ in ACCESS_DOT..3 {
            self.ppu.tick_dot(&mut *self.mapper);
        }
        // One region clocks its picture at three and a fifth dots to the
        // processor's cycle rather than three, so every fifth cycle owes one
        // more. Without it a frame there runs six and a half percent long,
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
        // The one register inside the 2A03 itself. The processor answers it on
        // its own internal bus, and nothing is put on the external one. So
        // unlike every other read this leaves open bus holding whatever it
        // held before, and the status byte is not what a later read of an
        // undriven address sees.
        if addr == 0x4015 {
            let val = self.apu.read_status(self.internal_bus);
            // Reaches the processor's own bus, and no further.
            self.internal_bus = val;
            return val;
        }

        let val = match addr {
            0x0000..=0x1fff => self.ram[addr as usize & 0x07ff],
            0x2000..=0x3fff => self.ppu.read_register(addr, &mut *self.mapper, self.open_bus),
            0x4016 => (self.open_bus & 0xe0) | self.read_controller(0),
            0x4017 => (self.open_bus & 0xe0) | self.read_controller(1),
            // The rest of the $4000 block is write-only and reads as open bus.
            0x4000..=0x401f => self.open_bus,
            _ => self.mapper.cpu_read(addr).unwrap_or(self.open_bus),
        };
        self.open_bus = val;
        self.internal_bus = val;
        val
    }

    fn write_raw(&mut self, addr: u16, val: u8) {
        self.open_bus = val;
        self.internal_bus = val;
        match addr {
            0x0000..=0x1fff => self.ram[addr as usize & 0x07ff] = val,
            0x2000..=0x3fff => self.ppu.write_register(addr, val, &mut *self.mapper),
            0x4014 => {
                self.sprite_dma_page = val;
                // The DMC's request is raised at the start of a cycle, this
                // 'write' at the end, so its halt begins a cycle later.
                self.oam = Oam::Asked;
            }
            0x4016 => self.strobe_pending = Some(val),
            0x4000..=0x4017 => self.apu.write_register(addr, val),
            0x4018..=0x401f => {}
            _ => {
                self.mapper.cpu_write(addr, val);
                // The only thing that ever moves the nametable wiring is a
                // 'write' to the cartridge, so it is taken here rather than
                // asked for at every read of one.
                self.ppu.set_mirroring(self.mapper.mirroring());
            }
        }
    }
}

impl Bus {
    /// A whole read cycle: the dots before the access, the access, and the
    /// dots that complete it.
    ///
    /// A read of `$2002` is the one access the PPU feels at two moments, and
    /// the only reason this is not simply the three lines it looks like. Bit 7
    /// has a latch behind it, filled and the flag it copied cleared as `M2`
    /// goes high, where the access lands. The two sprite flags have no latch:
    /// they are whatever they are when `M2` goes low again — 1.875 dots later
    /// on a revision G chip, which is this cycle's last dot. So for that one
    /// address the access is taken after the cycle's dots rather than between
    /// them, and reports flags the rest of the machine has moved on to.
    fn read_cycle(&mut self, addr: u16, access: impl FnOnce(&mut Self) -> u8) -> u8 {
        self.tick_before();
        if Self::is_status(addr) {
            self.ppu.begin_status_read();
            self.tick_after();
            return access(self);
        }
        let val = access(self);
        self.tick_after();
        val
    }

    /// Whether an address is the PPU's status register, mirrors included.
    fn is_status(addr: u16) -> bool {
        (0x2000..=0x3fff).contains(&addr) && addr & 7 == 2
    }

    /// Which half of the clock the cycle with this count falls on.
    fn phase(count: u64) -> Phase {
        if count & 1 == GET_PHASE {
            Phase::Get
        } else {
            Phase::Put
        }
    }

    /// One step of the sprite transfer: read on the even steps, write on the
    /// odd ones, five hundred and twelve of them.
    fn move_sprite_byte(&mut self, step: u16, read_addr: u16) {
        if step & 1 == 0 {
            let addr = ((self.sprite_dma_page as u16) << 8) | (step >> 1);
            self.dma_latch = self.dma_read(addr, read_addr);
        } else {
            let val = self.dma_latch;
            self.write(0x2004, val);
        }
        self.oam = if step + 1 == 0x200 { Oam::Idle } else { Oam::Moving(step + 1) };
    }
}

impl CpuBus for Bus {
    fn read(&mut self, addr: u16) -> u8 {
        self.read_cycle(addr, |bus| bus.read_raw(addr))
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
        self.dmc != Dmc::Idle || matches!(self.oam, Oam::Asked | Oam::Aligning | Oam::Moving(_))
    }

    /// Advance the DMA by one cycle, so the CPU keeps polling its interrupt
    /// lines throughout. `read_addr` is what it was about to read.
    ///
    /// One call is one CPU cycle. Each cycle is either spent taking the bus
    /// off the processor or spent moving a byte, and the two requesters make
    /// that choice separately — which is the whole subtlety here. Parity is
    /// read one cycle ahead, because it is the cycle about to run that
    /// matters and [`Bus::tick_before`], inside the access below, is what
    /// advances the count.
    fn dma_step(&mut self, read_addr: u16) {
        let get = Self::phase(self.cycle_count.wrapping_add(1)) == Phase::Get;
        // Cycles spent taking the bus. Both requesters can be spending one at
        // the same time, and neither moves anything on its own: all a halt or
        // a dummy has to achieve is that the processor is off the bus by the
        // end of it. The fetch spends two, the sprite transfer one.
        let dmc_taking = matches!(self.dmc, Dmc::Asked | Dmc::Dummy);
        let oam_taking = self.oam == Oam::Asked;
        if dmc_taking {
            self.dmc = if self.dmc == Dmc::Asked { Dmc::Dummy } else { Dmc::Aligning };
        }
        if oam_taking {
            self.oam = Oam::Aligning;
        }

        // The fetch, which outranks the sprite transfer for a get cycle — but
        // only once it is past taking the bus.
        if !dmc_taking && self.dmc == Dmc::Aligning && get {
            self.dmc = Dmc::Idle;
            let addr = self.apu.dmc_read_address();
            let val = self.dma_read(addr, read_addr);
            self.apu.load_dmc_sample(val);
            return;
        }

        // The sprite transfer begins on a get cycle and then alternates, so
        // its step's parity and the cycle's agree. A fetch taking a get out
        // from under it breaks that, and then it waits for them to line up
        // again.
        //
        // It may use a cycle the fetch is only spending to take the bus:
        // the processor is already off the bus, so there is nothing for that
        // halt to achieve and the transfer loses nothing to it. This is why a
        // fetch during a sprite transfer costs the processor two cycles
        // rather than four — the get the fetch reads on, and the one the
        // transfer then needs to get back into phase.
        if !oam_taking {
            match self.oam {
                Oam::Aligning if get => {
                    self.oam = Oam::Moving(0);
                    self.move_sprite_byte(0, read_addr);
                    return;
                }
                Oam::Moving(step) if (step & 1 == 0) == get => {
                    self.move_sprite_byte(step, read_addr);
                    return;
                }
                _ => {}
            }
        }

        // Nothing moved: a halt, a dummy, or a cycle spent waiting for the
        // parity a transfer needs. All of them read wherever the processor's
        // address bus is still pointing.
        self.dma_read(read_addr, read_addr);
    }
}

impl Nes {
    /// The console's two kilobytes of work RAM, as the CPU sees it at $0000.
    ///
    /// Read-only, and for looking rather than running: a debugger, a tracer,
    /// or a test ROM that leaves its findings in memory rather than on screen.
    pub fn ram(&self) -> &[u8; 0x0800] {
        &self.bus.ram
    }

    /// A cartridge-space byte as the processor would see it, but without the
    /// read happening.
    ///
    /// `$4020...=$FFFF` only, and for looking rather than running, the same as
    /// [`Nes::ram`]: a debugger, or a test ROM that leaves its findings in
    /// cartridge RAM and expects to be read out rather than watched. Floating
    /// bus reads back as zero here rather than as whatever was last driven,
    /// since nothing is being driven.
    pub fn peek(&self, addr: u16) -> u8 {
        self.bus.mapper.cpu_peek(addr).unwrap_or(0)
    }

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

    /// The table a front end is handed must agree with expanding one at a time.
    #[test]
    fn the_palette_table_covers_every_identity() {
        let table = palette_table();
        assert_eq!(table.len(), COLOR_IDS);
        for id in 0..COLOR_IDS {
            assert_eq!(table[id], expand(id as u16), "identity {id}");
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

    /// Anything that is not this build's 'save' state for this cartridge is
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

    /// Shifting the 'write' by an odd number of cycles must swap the alignment.
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
