//! Cycle-accurate Ricoh 2A03 CPU core (6502 without decimal mode).
//!
//! Timing comes from the bus, not a cycle table: every memory access the real
//! chip performs — including dummy reads at un-fixed indexed addresses and the
//! double write of a read-modify-write — is issued here, and each one advances
//! the rest of the machine by exactly one cycle. Instruction length therefore
//! falls out of the access pattern instead of being asserted separately.

/// The CPU's view of the machine. Both accessors must clock the whole system
/// by one CPU cycle, since that is what the real bus transaction costs.
pub trait CpuBus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);

    /// Level of the `/NMI` line this cycle (`true` = asserted).
    fn nmi(&self) -> bool;
    /// Wired-OR of every `/IRQ` source this cycle (`true` = asserted).
    fn irq(&self) -> bool;

    /// True while the DMA unit is holding the CPU off the bus. Checked before
    /// each instruction fetch so OAM/DMC DMA can steal cycles.
    fn dma_pending(&self) -> bool {
        false
    }
    /// Advance the DMA one cycle. `read_addr` is what the CPU was about to
    /// read, which the unit drives during halt and alignment cycles.
    fn dma_step(&mut self, read_addr: u16) {
        let _ = read_addr;
    }
}

pub const C: u8 = 0x01;
pub const Z: u8 = 0x02;
pub const I: u8 = 0x04;
pub const D: u8 = 0x08;
pub const B: u8 = 0x10;
pub const U: u8 = 0x20;
pub const V: u8 = 0x40;
pub const N: u8 = 0x80;

const NMI_VECTOR: u16 = 0xfffa;
const RESET_VECTOR: u16 = 0xfffc;
const IRQ_VECTOR: u16 = 0xfffe;

mod execute;
mod ops;
mod undocumented;

#[derive(Clone, Debug)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub p: u8,
    pub pc: u16,

    /// `/NMI` is edge-sensitive: once latched the request survives until it
    /// is serviced, whatever the line does afterward.
    need_nmi: bool,
    prev_need_nmi: bool,
    prev_nmi_flag: bool,

    /// `/IRQ` is level-sensitive and masked by I. Both act on the value
    /// sampled a cycle earlier, giving the chip's second-to-last-cycle poll.
    run_irq: bool,
    prev_run_irq: bool,

    /// Total CPU cycles since power-on, for tracing and test harnesses.
    pub cycles: u64,

    /// Whether the DMA unit held the processor off the bus during the most
    /// recent read.
    ///
    /// The `RDY` line is invisible to a program everywhere but one place: the
    /// unstable stores lose their address-high-byte term when the cycle before
    /// their write is stolen. See `Cpu::unstable_store_at`.
    ///
    /// Only meaningful within the instruction that set it, so a snapshot has
    /// no reason to carry it.
    #[cfg_attr(feature = "snapshot", serde(skip))]
    held_off: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub const fn new() -> Self {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            // S powers up at zero; the reset sequence's three false stack
            // cycles are what leave it at the familiar $FD.
            s: 0,
            p: I | U,
            pc: 0,
            need_nmi: false,
            prev_need_nmi: false,
            prev_nmi_flag: false,
            run_irq: false,
            prev_run_irq: false,
            cycles: 0,
            held_off: false,
        }
    }

    #[inline]
    fn flag(&self, mask: u8) -> bool {
        self.p & mask != 0
    }

    #[rustfmt::skip]
    #[inline]
    fn set_flag(&mut self, mask: u8, on: bool) {
        if on { self.p |= mask; } else { self.p &= !mask; }
    }

    #[inline]
    fn set_zn(&mut self, val: u8) {
        self.set_flag(Z, val == 0);
        self.set_flag(N, val & 0x80 != 0);
    }

    // ---- Bus access: every access polls the interrupt lines afterward, so
    // `prev_run_irq` lags one cycle behind when an instruction ends.

    #[inline]
    fn read(&mut self, bus: &mut impl CpuBus, addr: u16) -> u8 {
        // A DMA can only halt a read; a 'write' already drives the bus. The CPU
        // keeps polling while halted, so step a cycle at a time.
        self.held_off = false;
        while bus.dma_pending() {
            bus.dma_step(addr);
            self.cycles += 1;
            self.poll(bus);
            self.held_off = true;
        }
        let val = bus.read(addr);
        self.cycles += 1;
        self.poll(bus);
        val
    }

    #[inline]
    fn write(&mut self, bus: &mut impl CpuBus, addr: u16, val: u8) {
        bus.write(addr, val);
        self.cycles += 1;
        self.poll(bus);
    }

    /// Sample both interrupt lines at the end of a cycle.
    #[inline]
    fn poll(&mut self, bus: &impl CpuBus) {
        // The NMI request goes high during the cycle *after* the edge is seen,
        // and stays high until it is handled.
        self.prev_need_nmi = self.need_nmi;
        let nmi = bus.nmi();
        if nmi && !self.prev_nmi_flag {
            self.need_nmi = true;
        }
        self.prev_nmi_flag = nmi;

        self.prev_run_irq = self.run_irq;
        self.run_irq = bus.irq() && !self.flag(I);
    }

    #[inline]
    fn fetch(&mut self, bus: &mut impl CpuBus) -> u8 {
        let val = self.read(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        val
    }

    #[inline]
    fn fetch_u16(&mut self, bus: &mut impl CpuBus) -> u16 {
        let lo = self.fetch(bus) as u16;
        let hi = self.fetch(bus) as u16;
        (hi << 8) | lo
    }

    #[inline]
    fn push(&mut self, bus: &mut impl CpuBus, val: u8) {
        let addr = 0x0100 | self.s as u16;
        self.write(bus, addr, val);
        self.s = self.s.wrapping_sub(1);
    }

    #[inline]
    fn pull(&mut self, bus: &mut impl CpuBus) -> u8 {
        self.s = self.s.wrapping_add(1);
        let addr = 0x0100 | self.s as u16;
        self.read(bus, addr)
    }

    // ---- Reset and interrupts -------------------------------------------

    /// Drive `/RES`. Consumes the seven cycles the real sequence takes.
    pub fn reset(&mut self, bus: &mut impl CpuBus) {
        self.read(bus, self.pc);
        self.read(bus, self.pc);
        // Reset performs the same three stack cycles as an interrupt but with
        // the write line disabled, so it only decrements S.
        for _ in 0..3 {
            self.read(bus, 0x0100 | self.s as u16);
            self.s = self.s.wrapping_sub(1);
        }
        self.set_flag(I, true);
        let lo = self.read(bus, RESET_VECTOR) as u16;
        let hi = self.read(bus, RESET_VECTOR + 1) as u16;
        self.pc = (hi << 8) | lo;
        self.need_nmi = false;
        self.prev_need_nmi = false;
        self.run_irq = false;
        self.prev_run_irq = false;
    }

    /// The seven-cycle hardware interrupt sequence.
    fn interrupt(&mut self, bus: &mut impl CpuBus) {
        // Two cycles are spent fetching and discarding at PC: the opcode
        // register is forced to BRK and the PC increment suppressed.
        self.read(bus, self.pc);
        self.read(bus, self.pc);
        self.push(bus, (self.pc >> 8) as u8);
        self.push(bus, self.pc as u8);

        // The vector is chosen here, not when the request was latched, so an
        // NMI arriving during the push phase hijacks an IRQ already underway.
        let vector = if self.need_nmi {
            self.need_nmi = false;
            NMI_VECTOR
        } else {
            IRQ_VECTOR
        };

        self.push(bus, (self.p | U) & !B);
        self.set_flag(I, true);

        let lo = self.read(bus, vector) as u16;
        let hi = self.read(bus, vector + 1) as u16;
        self.pc = (hi << 8) | lo;
    }

    /// Execute an instruction, then service any pending interrupt — after it,
    /// never instead of it, so a hijacking NMI still runs the handler's first.
    pub fn step(&mut self, bus: &mut impl CpuBus) {
        let opcode = self.fetch(bus);
        self.execute(bus, opcode);

        if self.prev_run_irq || self.prev_need_nmi {
            self.interrupt(bus);
        }
    }
}

