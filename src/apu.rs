//! The 2A03's audio unit: two pulses, a triangle, a noise channel and a DMC.
//!
//! Everything is clocked from the CPU cycle. The frame counter — which is a
//! sequencer, not a "frame" in any video sense — drives envelope, sweep and
//! length updates and is one of the console's two IRQ sources, so it matters
//! even to programs that produce no sound at all.

extern crate alloc;
use alloc::vec::Vec;

use crate::cartridge::TvSystem;
use crate::Phase;

/// The half of the processor's clock the APU counts its own cycles on.
///
/// The pulse and noise timers advance here, and so does anything the unit
/// measures in its own cycles rather than the processor's — including how
/// long a write to `$4017` takes to be felt. Naming it once is what keeps
/// those in step: a write landing on this phase is felt a cycle sooner than
/// one landing between two of them, and that only means anything if "this
/// phase" is the same phase in both places.
const APU_CLOCK: Phase = Phase::Put;

/// Length values indexed by the 5-bit field of `$4003`-style registers.
const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

const TRIANGLE_TABLE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

const NOISE_PERIOD_NTSC: [u16; 16] =
    [4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068];
const NOISE_PERIOD_PAL: [u16; 16] =
    [4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778];

const DMC_PERIOD_NTSC: [u16; 16] =
    [428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54];
const DMC_PERIOD_PAL: [u16; 16] =
    [398, 354, 316, 298, 276, 236, 210, 198, 176, 148, 132, 118, 98, 78, 66, 50];

/// Output sample rate. Chosen to be a common device rate so frontends can hand
/// the buffer straight to the audio API without resampling.
pub const SAMPLE_RATE: u32 = 48_000;

/// How far the second channel lags the first, in milliseconds, by default.
///
/// The console is mono: one DAC, one signal. Sent to both speakers unaltered
/// it sits in a point between them, and a few milliseconds of delay on one
/// side is enough for the ear to place the two arrivals apart and hear width
/// instead — the precedence effect a stereo widener works on. Three or four
/// is the useful window; below two it reads as a comb rather than a space,
/// and past ten the arrivals separate into an echo.
///
/// This is presentation, not emulation, which is why it is a knob and not a
/// constant of the machine — see [`Apu::set_stereo_delay`].
pub const STEREO_DELAY_MS: f32 = 3.5;

/// The longest delay the line will hold: twenty milliseconds.
const STEREO_DELAY_MAX_MS: f32 = 20.0;

// ---- Shared units -------------------------------------------------------

#[derive(Clone, Default)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct Envelope {
    start: bool,
    constant: bool,
    volume: u8,
    divider: u8,
    decay: u8,
}

impl Envelope {
    /// `loop_flag` is the channel's length-counter halt bit, which doubles as
    /// the envelope's loop control.
    fn clock(&mut self, loop_flag: bool) {
        if self.start {
            self.start = false;
            self.decay = 15;
            self.divider = self.volume;
        } else if self.divider == 0 {
            self.divider = self.volume;
            if self.decay > 0 {
                self.decay -= 1;
            } else if loop_flag {
                self.decay = 15;
            }
        } else {
            self.divider -= 1;
        }
    }

    #[rustfmt::skip]
    fn output(&self) -> u8 {
        if self.constant { self.volume } else { self.decay }
    }
}

/// A channel's length counter. Halt and reload both apply a step late, after
/// the sequencer has clocked the counter, which is observable.
#[derive(Clone, Default)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct LengthCounter {
    value: u8,
    halt: bool,
    enabled: bool,
    /// Halt flag as written, waiting to be applied.
    new_halt: bool,
    /// Pending length, or zero when nothing is pending. The lookup table has
    /// no zero entries, so zero is unambiguous.
    reload_value: u8,
    /// Counter value at the moment the pending length was written.
    previous_value: u8,
}

impl LengthCounter {
    /// Soft reset. The triangle's counter is exempt and is handled by the
    /// caller, because reset leaves it untouched.
    fn reset(&mut self) {
        self.enabled = false;
        self.halt = false;
        self.new_halt = false;
        self.value = 0;
        self.reload_value = 0;
        self.previous_value = 0;
    }

    /// Queue a new halt flag, to be applied by [`LengthCounter::reload`].
    fn set_halt(&mut self, halt: bool) {
        self.new_halt = halt;
    }

    /// Apply anything queued. Called once per APU step, after the sequencer.
    fn reload(&mut self) {
        if self.reload_value != 0 {
            // If the counter moved since the write, the write is discarded.
            if self.value == self.previous_value {
                self.value = self.reload_value;
            }
            self.reload_value = 0;
        }
        self.halt = self.new_halt;
    }

    fn clock(&mut self) {
        if self.value > 0 && !self.halt {
            self.value -= 1;
        }
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.value = 0;
        }
    }

    fn load(&mut self, index: u8) {
        if self.enabled {
            self.reload_value = LENGTH_TABLE[index as usize & 0x1f];
            self.previous_value = self.value;
        }
    }

    fn active(&self) -> bool {
        self.value > 0
    }
}

// ---- Pulse --------------------------------------------------------------

#[derive(Clone, Default)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct Pulse {
    /// Pulse 1 and 2 differ only in how the sweep unit computes a negative
    /// target: channel 1 uses one's complement, channel 2 two's complement.
    ones_complement: bool,
    duty: u8,
    sequence_pos: u8,
    timer: u16,
    timer_period: u16,
    envelope: Envelope,
    length: LengthCounter,

    sweep_enabled: bool,
    sweep_negate: bool,
    sweep_period: u8,
    sweep_shift: u8,
    sweep_divider: u8,
    sweep_reload: bool,
}

impl Pulse {
    fn tick(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            self.sequence_pos = (self.sequence_pos + 1) & 7;
        } else {
            self.timer -= 1;
        }
    }

    fn sweep_target(&self) -> u16 {
        let change = self.timer_period >> self.sweep_shift;
        if self.sweep_negate {
            let change = if self.ones_complement { change + 1 } else { change };
            self.timer_period.saturating_sub(change)
        } else {
            self.timer_period + change
        }
    }

    /// A period below 8, or a sweep target that would overflow 11 bits, gates
    /// the channel off even when the sweep unit itself is disabled.
    fn muted(&self) -> bool {
        self.timer_period < 8 || self.sweep_target() > 0x7ff
    }

    fn clock_sweep(&mut self) {
        if self.sweep_divider == 0 && self.sweep_enabled && self.sweep_shift > 0 && !self.muted() {
            self.timer_period = self.sweep_target();
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.length.active() || self.muted() {
            return 0;
        }
        if DUTY_TABLE[self.duty as usize][self.sequence_pos as usize] == 0 {
            0
        } else {
            self.envelope.output()
        }
    }
}

// ---- Triangle -----------------------------------------------------------

#[derive(Clone, Default)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct Triangle {
    timer: u16,
    timer_period: u16,
    sequence_pos: u8,
    length: LengthCounter,
    linear_counter: u8,
    linear_reload_value: u8,
    linear_reload: bool,
    control: bool,
}

impl Triangle {
    /// Unlike the other channels the triangle steps at the full CPU rate.
    fn tick(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            if self.length.active() && self.linear_counter > 0 {
                self.sequence_pos = (self.sequence_pos + 1) & 31;
            }
        } else {
            self.timer -= 1;
        }
    }

    fn clock_linear(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_reload_value;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control {
            self.linear_reload = false;
        }
    }

    fn output(&self) -> u8 {
        // Periods of 0 and 1 produce an inaudible ultrasonic tone; holding the
        // output steady avoids a popping artefact without changing behaviour.
        if self.timer_period < 2 {
            return 7;
        }
        TRIANGLE_TABLE[self.sequence_pos as usize]
    }
}

// ---- Noise --------------------------------------------------------------

#[derive(Clone)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct Noise {
    shift: u16,
    mode: bool,
    timer: u16,
    timer_period: u16,
    envelope: Envelope,
    length: LengthCounter,
}

impl Default for Noise {
    fn default() -> Self {
        // The shift register powers up holding 1; a zero register never moves.
        Noise {
            shift: 1,
            mode: false,
            timer: 0,
            timer_period: 0,
            envelope: Envelope::default(),
            length: LengthCounter::default(),
        }
    }
}

impl Noise {
    fn tick(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            let tap = if self.mode { 6 } else { 1 };
            let feedback = (self.shift & 1) ^ ((self.shift >> tap) & 1);
            self.shift = (self.shift >> 1) | (feedback << 14);
        } else {
            self.timer -= 1;
        }
    }

    #[rustfmt::skip]
    fn output(&self) -> u8 {
        if self.shift & 1 != 0 || !self.length.active() { 0 } else { self.envelope.output() }
    }
}

// ---- DMC ----------------------------------------------------------------

#[derive(Clone)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct Dmc {
    irq_enabled: bool,
    loop_flag: bool,

    /// One less than the tabulated rate, because the cycle on which the timer
    /// expires is itself part of the period.
    timer_period: u16,
    timer: u16,
    output: u8,

    sample_addr: u16,
    sample_len: u16,
    current_addr: u16,
    bytes_remaining: u16,

    /// The one-byte sample buffer sitting between the DMA and the shift
    /// register. Refilled the moment the shift register takes its contents.
    read_buffer: u8,
    buffer_empty: bool,

    shift: u8,
    bits_remaining: u8,
    silence: bool,

    irq_flag: bool,

    /// $4015 enables and disables take effect a couple of cycles late, so a
    /// DMA already under way still steals its cycle.
    transfer_start_delay: u8,
    disable_delay: u8,

    /// Set when the channel wants the bus to fetch `current_addr`.
    wants_transfer: bool,

    /// Set when a fetch already under way is to be given up. A disable that
    /// arrives mid-fetch does not wait for it: the cycles it has already
    /// taken off the processor are gone, but the read never happens.
    drops_transfer: bool,
}

impl Default for Dmc {
    fn default() -> Self {
        Dmc {
            irq_enabled: false,
            loop_flag: false,
            timer_period: DMC_PERIOD_NTSC[0] - 1,
            // Start the timer full so the channel does not clock on the very
            // first cycle out of reset.
            timer: DMC_PERIOD_NTSC[0] - 1,
            output: 0,
            sample_addr: 0xc000,
            sample_len: 1,
            current_addr: 0,
            bytes_remaining: 0,
            read_buffer: 0,
            buffer_empty: true,
            shift: 0,
            bits_remaining: 8,
            silence: true,
            irq_flag: false,
            transfer_start_delay: 0,
            disable_delay: 0,
            wants_transfer: false,
            drops_transfer: false,
        }
    }
}

impl Dmc {
    fn init_sample(&mut self) {
        self.current_addr = self.sample_addr;
        self.bytes_remaining = self.sample_len;
    }

    /// Ask the bus for the next sample byte, if one is actually wanted.
    fn start_transfer(&mut self) {
        if self.buffer_empty && self.bytes_remaining > 0 {
            self.wants_transfer = true;
        }
    }

    /// Housekeeping that runs every CPU cycle, independent of the timer.
    fn process_clock(&mut self) {
        if self.disable_delay > 0 {
            self.disable_delay -= 1;
            if self.disable_delay == 0 {
                self.bytes_remaining = 0;
                // Abandon the fetch whether or not it has started. One that
                // had not been taken up simply never is; one that has is cut
                // short where it stands, which is how a fetch ends up
                // costing the processor a single cycle.
                self.wants_transfer = false;
                self.drops_transfer = true;
            }
        }
        if self.transfer_start_delay > 0 {
            self.transfer_start_delay -= 1;
            if self.transfer_start_delay == 0 {
                self.start_transfer();
            }
        }
    }

    /// Advance the output unit by one CPU cycle.
    fn run(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
            return;
        }
        self.timer = self.timer_period;

        if !self.silence {
            // The delta counter saturates rather than wrapping, so a sample
            // cannot snap the output from one extreme to the other.
            if self.shift & 1 != 0 {
                if self.output <= 125 {
                    self.output += 2;
                }
            } else if self.output >= 2 {
                self.output -= 2;
            }
        }
        self.shift >>= 1;

        self.bits_remaining -= 1;
        if self.bits_remaining == 0 {
            self.bits_remaining = 8;
            if self.buffer_empty {
                self.silence = true;
            } else {
                self.silence = false;
                self.shift = self.read_buffer;
                self.buffer_empty = true;
                // Refill as soon as the buffer empties, unless the channel was
                // only just enabled and the delay above owns the first fetch.
                if self.transfer_start_delay == 0 {
                    self.start_transfer();
                }
            }
        }
    }

    /// Accept the byte the bus fetched.
    fn set_read_buffer(&mut self, val: u8) {
        if self.bytes_remaining == 0 {
            return;
        }
        self.read_buffer = val;
        self.buffer_empty = false;
        self.current_addr = self.current_addr.wrapping_add(1);
        if self.current_addr == 0 {
            self.current_addr = 0x8000;
        }
        self.bytes_remaining -= 1;
        if self.bytes_remaining == 0 {
            if self.loop_flag {
                // A looping sample never raises the interrupt.
                self.init_sample();
            } else if self.irq_enabled {
                self.irq_flag = true;
            }
        }
    }

    /// $4015 bit 4. Neither an enable nor a disable is felt where it lands:
    /// the channel is only reached two or three cycles later, and which of
    /// the two depends on the phase the write arrived on. `phase` says
    /// whether that was a cycle a transfer could have read on, and those are
    /// the writes that wait the longer time.
    ///
    /// The difference is a whole cycle of stall, because it decides which
    /// phase the fetch this write provokes ends up halting on — and a fetch
    /// that halts on a get cycle needs no alignment, so it runs three cycles
    /// instead of four.
    fn set_enabled(&mut self, enabled: bool, phase: Phase) {
        if !enabled {
            if self.disable_delay == 0 {
                self.disable_delay = if phase == Phase::Get { 3 } else { 2 };
            }
        } else if self.bytes_remaining == 0 {
            self.init_sample();
            self.transfer_start_delay = if phase == Phase::Get { 3 } else { 2 };
        }
    }
}

// ---- The APU ------------------------------------------------------------

#[derive(Clone)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub struct Apu {
    pulse1: Pulse,
    pulse2: Pulse,
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,

    tv: TvSystem,
    cycle: u64,
    /// Alternates every CPU cycle. Half the APU is clocked on one phase only,
    /// and several register writes behave differently depending on it.
    /// Which half of the processor's clock the cycle now running falls on.
    ///
    /// The processor's divider is the APU's own, so this is not counted here
    /// — the bus hands it over, and there is one divider in the machine
    /// rather than two that can drift apart. Everything in here that cares
    /// says so in the same terms the hardware documentation does: a get
    /// cycle, a put cycle, or the edge between two of them.
    phase: Phase,

    // ---- Frame sequencer ----
    /// Position within the six-entry step table.
    step: usize,
    /// CPU cycles elapsed within the current sequence.
    step_cycle: i32,
    /// 0 for the four-step sequence, 1 for the five-step one.
    step_mode: usize,
    inhibit_irq: bool,
    /// Status-register flag, which is not quite the same thing as the level of
    /// the interrupt line: an inhibited sequencer still sets it briefly.
    frame_irq_flag: bool,
    frame_irq_line: bool,
    /// A read of `$4015` has asked for the frame interrupt flag to go.
    ///
    /// The clear does not happen where the read does: it waits for the APU's
    /// next get cycle. Two reads in a row therefore see the flag twice if the
    /// first of them landed on a get — which is what `SLO $4015` arranges, and
    /// how a program can tell the console's phase.
    frame_irq_clearing: bool,
    /// A tick just occurred, so a `$4017` write must not produce another one
    /// on this cycle or the next.
    block_tick: u8,
    /// Value waiting to be applied to `$4017`, and the countdown until it is.
    pending_4017: i16,
    write_delay: i8,

    /// Sound waiting to be taken away. Not state: it is what the last frames
    /// produced, and the next frame produces more. Left out of a save state.
    #[cfg_attr(feature = "snapshot", serde(skip))]
    samples: Vec<f32>,
    /// Fractional accumulator for downsampling the ~1.79 MHz output stream.
    sample_accum: f64,
    sample_step: f64,
    filter: OutputFilter,
    /// The delay line the second channel reads from; see [`STEREO_DELAY_MS`].
    ///
    /// Not state, and skipped by a save state: at worst a few milliseconds of
    /// sound is missing from the far channel after a load.
    #[cfg_attr(feature = "snapshot", serde(skip))]
    widen: Widen,
}

impl Apu {
    pub fn new(tv: TvSystem) -> Self {
        let cpu_hz = match tv {
            TvSystem::Ntsc => 1_789_773.0,
            TvSystem::Pal => 1_662_607.0,
            TvSystem::Dendy => 1_773_448.0,
        };
        Apu {
            pulse1: Pulse { ones_complement: true, ..Pulse::default() },
            pulse2: Pulse::default(),
            triangle: Triangle::default(),
            noise: Noise::default(),
            dmc: Dmc::default(),
            tv,
            cycle: 0,
            phase: Phase::Put,
            step: 0,
            step_cycle: 0,
            step_mode: 0,
            inhibit_irq: false,
            frame_irq_flag: false,
            frame_irq_line: false,
            frame_irq_clearing: false,
            block_tick: 0,
            pending_4017: -1,
            write_delay: -1,
            samples: Vec::new(),
            sample_accum: 0.0,
            sample_step: SAMPLE_RATE as f64 / cpu_hz,
            filter: OutputFilter::new(SAMPLE_RATE),
            widen: Widen::new(STEREO_DELAY_MS, SAMPLE_RATE),
        }
    }

    /// Soft reset: the sequencer keeps its mode, the triangle's counter is
    /// untouched, and the DMC falls silent.
    pub fn reset(&mut self) {
        self.pulse1.length.reset();
        self.pulse2.length.reset();
        self.noise.length.reset();
        // "At reset, length counters should be enabled, triangle unaffected."
        self.triangle.length.enabled = false;

        self.dmc.bytes_remaining = 0;
        self.dmc.output = 0;
        self.dmc.irq_flag = false;
        self.dmc.silence = true;
        self.dmc.buffer_empty = true;
        self.dmc.bits_remaining = 8;

        self.frame_irq_flag = false;
        self.frame_irq_line = false;
        self.frame_irq_clearing = false;
        self.inhibit_irq = false;
        self.step = 0;
        // The sequencer is already running by the time the first instruction
        // does: out of reset the APU behaves as though `$4017` had been
        // written with `$00` some nine to twelve cycles beforehand, and a
        // write takes three or four of those to reach the sequencer, so what
        // is left over is spent before anything can observe it. Queueing the
        // write here instead would start the count late, and blargg's
        // `09.reset_timing` reports exactly that: fourth step too late.
        self.step_cycle = 0;
        self.pending_4017 = -1;
        self.write_delay = -1;
        self.block_tick = 0;
        self.filter.reset();
        self.widen.reset();
    }

    pub fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    pub fn irq(&self) -> bool {
        self.frame_irq_line || self.dmc.irq_flag
    }

    pub fn tick(&mut self, phase: Phase) {
        self.cycle += 1;
        self.phase = phase;

        // A read of `$4015` asked for the frame interrupt flag; it goes here,
        // on the next get cycle, rather than where the read happened. Applied
        // before the sequencer, so a cycle that is asserting the flag wins and
        // a read taken during the assertion clears nothing.
        if self.phase == Phase::Get && core::mem::take(&mut self.frame_irq_clearing) {
            self.frame_irq_flag = false;
        }

        // Pulse and noise timers are clocked every second CPU cycle; the
        // triangle and the DMC run at the full rate.
        if self.phase == APU_CLOCK {
            self.pulse1.tick();
            self.pulse2.tick();
            self.noise.tick();
        }
        self.triangle.tick();
        self.dmc.process_clock();
        self.dmc.run();

        self.clock_frame_counter();
        // Queued lengths and halt flags land after the sequencer has had its
        // chance to clock the counters.
        self.pulse1.length.reload();
        self.pulse2.length.reload();
        self.triangle.length.reload();
        self.noise.length.reload();
        self.accumulate_sample();
    }

    fn clock_frame_counter(&mut self) {
        // Step boundaries in CPU cycles. The four-step sequence spends its
        // last three cycles asserting the interrupt, hence six entries.
        let steps: &[i32; 6] = match (self.tv, self.step_mode) {
            (TvSystem::Pal, 0) => &[8313, 16627, 24939, 33252, 33253, 33254],
            (TvSystem::Pal, _) => &[8313, 16627, 24939, 33253, 41565, 41566],
            (_, 0) => &[7457, 14913, 22371, 29828, 29829, 29830],
            (_, _) => &[7457, 14913, 22371, 29829, 37281, 37282],
        };
        // Steps 1 and 4 clock the length counters and sweeps as well as the
        // envelopes; steps 3 and 5 do nothing but carry the interrupt.
        const HALF: [bool; 6] = [false, true, false, false, true, false];
        const TICKS: [bool; 6] = [true, true, true, false, true, false];

        self.step_cycle += 1;
        if self.step_cycle >= steps[self.step] {
            if self.step_mode == 0 && self.step >= 3 {
                self.frame_irq_flag = true;
                if !self.inhibit_irq {
                    self.frame_irq_line = true;
                } else if self.step == 5 {
                    // Inhibited, so the flag only ever flickers on.
                    self.frame_irq_flag = false;
                }
            }
            if TICKS[self.step] && self.block_tick == 0 {
                self.clock_quarter_frame();
                if HALF[self.step] {
                    self.clock_half_frame();
                }
                self.block_tick = 2;
            }
            self.step += 1;
            if self.step == 6 {
                self.step = 0;
                self.step_cycle = 0;
            }
        }

        if self.pending_4017 >= 0 {
            self.write_delay -= 1;
            if self.write_delay == 0 {
                self.step_mode = if self.pending_4017 & 0x80 != 0 { 1 } else { 0 };
                self.write_delay = -1;
                self.step = 0;
                self.step_cycle = 0;
                self.pending_4017 = -1;
                // Selecting the five-step sequence clocks everything at once,
                // which is how software forces an immediate length update.
                if self.step_mode == 1 && self.block_tick == 0 {
                    self.clock_quarter_frame();
                    self.clock_half_frame();
                    self.block_tick = 2;
                }
            }
        }

        if self.block_tick > 0 {
            self.block_tick -= 1;
        }
    }

    fn clock_quarter_frame(&mut self) {
        let (h1, h2, hn) =
            (self.pulse1.length.halt, self.pulse2.length.halt, self.noise.length.halt);
        self.pulse1.envelope.clock(h1);
        self.pulse2.envelope.clock(h2);
        self.noise.envelope.clock(hn);
        self.triangle.clock_linear();
    }

    fn clock_half_frame(&mut self) {
        self.pulse1.length.clock();
        self.pulse2.length.clock();
        self.triangle.length.clock();
        self.noise.length.clock();
        self.pulse1.clock_sweep();
        self.pulse2.clock_sweep();
    }

    /// The status byte at `$4015`.
    ///
    /// `bus` is what the processor's own data bus is holding. Bit 5 has nothing
    /// behind it — the register is only seven bits wide — so that bit floats
    /// with the bus rather than reading as zero. The processor's bus, not the
    /// external one: a transfer's read reaches that and not this.
    pub fn read_status(&mut self, bus: u8) -> u8 {
        let bit = |set: bool, mask: u8| if set { mask } else { 0 };
        let status = bit(self.pulse1.length.active(), 0x01)
            | bit(self.pulse2.length.active(), 0x02)
            | bit(self.triangle.length.active(), 0x04)
            | bit(self.noise.length.active(), 0x08)
            | bit(self.dmc.bytes_remaining > 0, 0x10)
            | (bus & 0x20)
            | bit(self.frame_irq_flag, 0x40)
            | bit(self.dmc.irq_flag, 0x80);
        // Reading acknowledges the frame interrupt but not the DMC's. The
        // line drops at once, but the flag this register reports only goes on
        // the next get cycle — see `frame_irq_clearing`.
        self.frame_irq_clearing = true;
        self.frame_irq_line = false;
        status
    }

    pub fn write_register(&mut self, addr: u16, val: u8) {
        match addr {
            0x4000 => {
                self.pulse1.duty = val >> 6;
                self.pulse1.length.set_halt(val & 0x20 != 0);
                self.pulse1.envelope.constant = val & 0x10 != 0;
                self.pulse1.envelope.volume = val & 0x0f;
            }
            0x4001 => {
                self.pulse1.sweep_enabled = val & 0x80 != 0;
                self.pulse1.sweep_period = (val >> 4) & 0x07;
                self.pulse1.sweep_negate = val & 0x08 != 0;
                self.pulse1.sweep_shift = val & 0x07;
                self.pulse1.sweep_reload = true;
            }
            0x4002 => {
                self.pulse1.timer_period = (self.pulse1.timer_period & 0x700) | val as u16;
            }
            0x4003 => {
                self.pulse1.timer_period =
                    (self.pulse1.timer_period & 0x0ff) | (((val & 0x07) as u16) << 8);
                self.pulse1.length.load(val >> 3);
                self.pulse1.sequence_pos = 0;
                self.pulse1.envelope.start = true;
            }

            0x4004 => {
                self.pulse2.duty = val >> 6;
                self.pulse2.length.set_halt(val & 0x20 != 0);
                self.pulse2.envelope.constant = val & 0x10 != 0;
                self.pulse2.envelope.volume = val & 0x0f;
            }
            0x4005 => {
                self.pulse2.sweep_enabled = val & 0x80 != 0;
                self.pulse2.sweep_period = (val >> 4) & 0x07;
                self.pulse2.sweep_negate = val & 0x08 != 0;
                self.pulse2.sweep_shift = val & 0x07;
                self.pulse2.sweep_reload = true;
            }
            0x4006 => {
                self.pulse2.timer_period = (self.pulse2.timer_period & 0x700) | val as u16;
            }
            0x4007 => {
                self.pulse2.timer_period =
                    (self.pulse2.timer_period & 0x0ff) | (((val & 0x07) as u16) << 8);
                self.pulse2.length.load(val >> 3);
                self.pulse2.sequence_pos = 0;
                self.pulse2.envelope.start = true;
            }

            0x4008 => {
                self.triangle.control = val & 0x80 != 0;
                self.triangle.length.set_halt(val & 0x80 != 0);
                self.triangle.linear_reload_value = val & 0x7f;
            }
            0x400a => {
                self.triangle.timer_period = (self.triangle.timer_period & 0x700) | val as u16;
            }
            0x400b => {
                self.triangle.timer_period =
                    (self.triangle.timer_period & 0x0ff) | (((val & 0x07) as u16) << 8);
                self.triangle.length.load(val >> 3);
                self.triangle.linear_reload = true;
            }

            0x400c => {
                self.noise.length.set_halt(val & 0x20 != 0);
                self.noise.envelope.constant = val & 0x10 != 0;
                self.noise.envelope.volume = val & 0x0f;
            }
            0x400e => {
                self.noise.mode = val & 0x80 != 0;
                let table = match self.tv {
                    TvSystem::Pal => &NOISE_PERIOD_PAL,
                    _ => &NOISE_PERIOD_NTSC,
                };
                self.noise.timer_period = table[val as usize & 0x0f];
            }
            0x400f => {
                self.noise.length.load(val >> 3);
                self.noise.envelope.start = true;
            }

            0x4010 => {
                self.dmc.irq_enabled = val & 0x80 != 0;
                self.dmc.loop_flag = val & 0x40 != 0;
                if !self.dmc.irq_enabled {
                    self.dmc.irq_flag = false;
                }
                let table = match self.tv {
                    TvSystem::Pal => &DMC_PERIOD_PAL,
                    _ => &DMC_PERIOD_NTSC,
                };
                self.dmc.timer_period = table[val as usize & 0x0f] - 1;
            }
            0x4011 => self.dmc.output = val & 0x7f,
            0x4012 => self.dmc.sample_addr = 0xc000 + ((val as u16) << 6),
            0x4013 => self.dmc.sample_len = ((val as u16) << 4) + 1,

            0x4015 => {
                self.pulse1.length.set_enabled(val & 0x01 != 0);
                self.pulse2.length.set_enabled(val & 0x02 != 0);
                self.triangle.length.set_enabled(val & 0x04 != 0);
                self.noise.length.set_enabled(val & 0x08 != 0);
                self.dmc.set_enabled(val & 0x10 != 0, self.phase);
                // Writing here always acknowledges the DMC interrupt.
                self.dmc.irq_flag = false;
            }
            0x4017 => {
                self.pending_4017 = val as i16;
                // The write takes effect three CPU cycles later, or four if it
                // landed between APU cycles rather than on one.
                // Felt three cycles on if the write landed on one of the
                // APU's own cycles, four if it landed between two of them.
                self.write_delay = if self.phase == APU_CLOCK { 3 } else { 4 };
                self.inhibit_irq = val & 0x40 != 0;
                if self.inhibit_irq {
                    self.frame_irq_flag = false;
                    self.frame_irq_line = false;
                }
            }
            _ => {}
        }
    }

    /// True once, when the DMC has just asked for a sample byte.
    pub(crate) fn take_dmc_request(&mut self) -> bool {
        core::mem::take(&mut self.dmc.wants_transfer)
    }

    /// Whether a fetch in flight is to be given up, because the channel was
    /// disabled while it was under way.
    pub(crate) fn take_dmc_abort(&mut self) -> bool {
        core::mem::take(&mut self.dmc.drops_transfer)
    }


    pub(crate) fn dmc_read_address(&self) -> u16 {
        self.dmc.current_addr
    }

    /// Called by the bus once it has fetched the byte the DMC asked for.
    pub(crate) fn load_dmc_sample(&mut self, byte: u8) {
        self.dmc.set_read_buffer(byte);
    }

    fn accumulate_sample(&mut self) {
        self.sample_accum += self.sample_step;
        if self.sample_accum < 1.0 {
            return;
        }
        self.sample_accum -= 1.0;
        let mixed = self.mix();
        self.samples.push(self.filter.apply(mixed));
    }

    /// How much louder the DMC sits than the hardware notes put it.
    ///
    /// Those weigh it 1/22638 against the triangle's 1/8227 and the noise's
    /// 1/12241, which is right and leaves the sampled drums and bass sitting
    /// under the melodic channels. This is the one place the mix departs from
    /// the notes, and it does so on purpose.
    const DMC_LIFT: f32 = 1.3;

    /// The non-linear mixer, using the closed forms from the hardware notes
    /// rather than the lookup tables, which agree to within a sample step.
    fn mix(&self) -> f32 {
        let pulse_sum = self.pulse1.output() as f32 + self.pulse2.output() as f32;
        let pulse = if pulse_sum == 0.0 { 0.0 } else { 95.88 / (8128.0 / pulse_sum + 100.0) };

        let t = self.triangle.output() as f32 / 8227.0;
        let n = self.noise.output() as f32 / 12241.0;
        let d = self.dmc.output as f32 * Self::DMC_LIFT / 22638.0;
        let denom = t + n + d;
        let tnd = if denom == 0.0 { 0.0 } else { 159.79 / (1.0 / denom + 100.0) };

        pulse + tnd
    }

    pub fn drain(&mut self, out: &mut Vec<f32>) {
        out.append(&mut self.samples);
    }

    /// Drain as interleaved stereo pairs, the second channel delayed.
    ///
    /// Two values per sample, left then right. The mono [`Apu::drain`] is
    /// untouched, so a front end that wants one channel keeps getting one.
    pub fn drain_stereo(&mut self, out: &mut Vec<f32>) {
        out.reserve(self.samples.len() * 2);
        for sample in self.samples.drain(..) {
            let (left, right) = self.widen.apply(sample);
            out.push(left);
            out.push(right);
        }
    }

    /// How far the second channel lags, in milliseconds. Zero for mono.
    pub fn set_stereo_delay(&mut self, ms: f32) {
        self.widen = Widen::new(ms, SAMPLE_RATE);
    }

    /// The delay currently in use, in milliseconds.
    pub fn stereo_delay(&self) -> f32 {
        self.widen.ms
    }
}

/// The analogue stage: a high-pass, and nothing else.
///
/// Both halves of the mixer are unsigned, so its output carries a standing DC
/// offset, and that is all this is here to remove.
///
/// The hardware had two more stages and neither is kept. A high-pass at
/// 440 Hz took 23.8 dB out at 55 Hz and 18.2 dB at 82 Hz — the whole range
/// the triangle plays bass lines in, so a cartridge did not sound the way it
/// was written. A low-pass at 14 kHz cost 1.2 dB at 8 kHz and 1.8 dB at
/// 10 kHz, which is the range that tells one voice from another. Both are
/// what a television of the day did to the sound; neither is what anybody
/// wants to hear it through now.
#[derive(Clone, Default)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct OutputFilter {
    high1: HighPass,
}

impl OutputFilter {
    fn new(sample_rate: u32) -> Self {
        OutputFilter {
            high1: HighPass::new(90.0, sample_rate),
        }
    }

    fn apply(&mut self, sample: f32) -> f32 {
        self.high1.apply(sample).clamp(-1.0, 1.0)
    }

    fn reset(&mut self) {
        self.high1.reset();
    }
}

/// Turns the console's one channel into two by holding one of them back.
///
/// A plain delay of a few milliseconds, which is all the effect needs.
#[derive(Clone)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct Widen {
    line: Vec<f32>,
    at: usize,
    ms: f32,
}

impl Default for Widen {
    fn default() -> Self {
        Widen::new(STEREO_DELAY_MS, SAMPLE_RATE)
    }
}

impl Widen {
    fn new(ms: f32, sample_rate: u32) -> Self {
        let ms = ms.clamp(0.0, STEREO_DELAY_MAX_MS);
        let lag = (ms * sample_rate as f32 / 1000.0) as usize;
        Widen {
            line: alloc::vec![0.0; lag],
            at: 0,
            ms,
        }
    }

    /// One mono sample in, a left and a right out.
    fn apply(&mut self, sample: f32) -> (f32, f32) {
        if self.line.is_empty() {
            return (sample, sample);
        }
        // Read before writing: what is under the index is the sample from a
        // whole line-length ago.
        let delayed = self.line[self.at];
        self.line[self.at] = sample;
        self.at = (self.at + 1) % self.line.len();
        (sample, delayed)
    }

    fn reset(&mut self) {
        self.line.iter_mut().for_each(|s| *s = 0.0);
        self.at = 0;
    }
}

/// A one-pole high-pass: `y = a * (y_prev + x - x_prev)`.
#[derive(Clone, Default)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
struct HighPass {
    a: f32,
    prev_in: f32,
    prev_out: f32,
}

impl HighPass {
    fn new(cutoff: f32, sample_rate: u32) -> Self {
        let rc = 1.0 / (core::f32::consts::TAU * cutoff);
        let dt = 1.0 / sample_rate as f32;
        HighPass { a: rc / (rc + dt), prev_in: 0.0, prev_out: 0.0 }
    }

    fn apply(&mut self, sample: f32) -> f32 {
        let out = self.a * (self.prev_out + sample - self.prev_in);
        self.prev_in = sample;
        self.prev_out = out;
        out
    }

    fn reset(&mut self) {
        self.prev_in = 0.0;
        self.prev_out = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the APU for a number of CPU cycles, alternating the bus phase the
    /// way the bus does.
    fn run(apu: &mut Apu, cycles: u32) {
        for i in 0..cycles {
            apu.tick(if i % 2 == 0 { Phase::Get } else { Phase::Put });
        }
    }

    /// Load a length on every channel and let it run down.
    fn load_all_lengths(apu: &mut Apu) {
        apu.write_register(0x4017, 0x00);
        apu.write_register(0x4015, 0x0f);
        // $18 selects length-table entry 3, which is a count of 2.
        apu.write_register(0x4003, 0x18);
        apu.write_register(0x4007, 0x18);
        apu.write_register(0x400b, 0x18);
        apu.write_register(0x400f, 0x18);
    }

    /// From blargg's `apu_reset/len_ctrs_enabled`: reset leaves the triangle's
    /// halt flag alone, so only its counter survives being clocked.
    #[test]
    fn reset_leaves_the_triangle_length_counter_alone() {
        let mut apu = Apu::new(TvSystem::Ntsc);

        // Halt the triangle and give every channel a length.
        apu.write_register(0x4000, 0x00);
        apu.write_register(0x4004, 0x00);
        apu.write_register(0x4008, 0xff); // triangle halt
        apu.write_register(0x400c, 0x00);
        load_all_lengths(&mut apu);
        run(&mut apu, 100);

        apu.reset();

        load_all_lengths(&mut apu);
        // Two frame sequences is long enough for a count of 2 to reach zero.
        run(&mut apu, 29830 * 2);

        assert_eq!(
            apu.read_status(0) & 0x0f,
            0x04,
            "only the triangle should still be counting after reset"
        );
    }

    /// The halt flag applies after the sequencer has clocked the counter, so a
    /// halt written just before a clock does not prevent that clock.
    #[test]
    fn length_halt_applies_after_the_counter_is_clocked() {
        let mut apu = Apu::new(TvSystem::Ntsc);
        apu.write_register(0x4017, 0x00);
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4003, 0x18); // pulse 1 length = 2
        run(&mut apu, 10);

        // Halt, then run past the first half-frame clock at cycle 14913.
        apu.write_register(0x4000, 0x20);
        run(&mut apu, 29830);
        assert_ne!(apu.read_status(0) & 0x01, 0, "a halted counter must not reach zero");
    }

    /// A length written while the counter is being clocked is discarded.
    #[test]
    fn length_reload_is_ignored_when_the_counter_moves() {
        let mut apu = Apu::new(TvSystem::Ntsc);
        apu.write_register(0x4017, 0x00);
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4003, 0x18);
        // Reach the cycle on which the sequencer clocks the length counters.
        run(&mut apu, 14913);
        apu.write_register(0x4003, 0x18);
        run(&mut apu, 1);
        // The counter moved between the write and the reload, so the reload
        // is dropped and the count stays where the clock left it.
        assert_eq!(apu.read_status(0) & 0x01, 1);
    }
}
