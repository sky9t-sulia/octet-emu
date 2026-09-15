//! The 2C02 picture processor.
//!
//! Modeled dot by dot. The scroll register layout, the shift-register
//! pipeline and the two-scanline-deep sprite evaluation are the real ones,
//! because the timing quirks games rely on — mid-frame scroll splits, sprite
//! zero hit, the pre-render dot skip — only fall out of that structure.

use alloc::boxed::Box;

use crate::cartridge::{Mirroring, TvSystem};
use crate::mapper::Mapper;

/// How many distinct colors a PPU can emit: sixty-four palette entries,
/// each under one of eight emphasis settings.
pub const COLOR_IDS: usize = 512;

pub const FRAME_WIDTH: usize = 256;
pub const FRAME_HEIGHT: usize = 240;

const DOTS_PER_SCANLINE: u16 = 341;

/// Stands in `sprite_soonest` for "not worked out yet". No real position can
/// reach it, so a dot that meets it knows to go and look.
const SPRITE_UNKNOWN: u16 = u16::MAX;

/// Dots between a `$2007` read ending and the PPU acting on it. The state
/// machine starts as `M2` falls; one dot later the address goes out.
pub(super) const FETCH_DELAY: u8 = 3;

/// The 2C02's 64-entry palette as sRGB.
///
/// The console emits composite, so any RGB table is an interpretation and
/// emulators differ. Entries a reference was found for are set to it; the rest
/// keep the values of the table they came from.
#[rustfmt::skip]
pub const PALETTE: [u32; 64] = [
    0x7c7c7c, 0x0000fc, 0x0000bc, 0x4428bc, 0x940084, 0xa80020, 0xa81000, 0x881400,
    0x503000, 0x007800, 0x006800, 0x005800, 0x004058, 0x000000, 0x000000, 0x000000,
    0xbcbcbc, 0x0078f8, 0x0058f8, 0x6844fc, 0xd800cc, 0xe40058, 0xf83800, 0xc84c0c,
    0xac7c00, 0x00b800, 0x00a800, 0x00a844, 0x008088, 0x000000, 0x000000, 0x000000,
    0xf8f8f8, 0x3cbcfc, 0x5c94fc, 0x9878f8, 0xf878f8, 0xf85898, 0xf87858, 0xfc9838,
    0xf8b800, 0x80d010, 0x58d854, 0x58f898, 0x00e8d8, 0x787878, 0x000000, 0x000000,
    0xfcfcfc, 0xa4e4fc, 0xb8b8f8, 0xd8b8f8, 0xf8b8f8, 0xf8a4c0, 0xfcbcb0, 0xfce0a8,
    0xf8d878, 0xd8f878, 0xb8f8b8, 0xb8f8d8, 0x00fcfc, 0xf8d8f8, 0x000000, 0x000000,
];

/// The RGB a color identity stands for — the inverse of what the PPU wrote
/// into [`Ppu::color_ids`].
///
/// A front end that carries identities rather than pixels expands them with
/// this, or with the table [`palette_table`] builds from it. Bits 0-5 are the
/// palette entry, bits 6-8 the `$2001` emphasis.
pub fn expand(id: u16) -> u32 {
    let color = PALETTE[(id & 0x3f) as usize];
    let emphasis = ((id >> 6) & 7) as u8;
    if emphasis == 0 {
        color
    } else {
        render::emphasize(color, emphasis)
    }
}

/// Every color a PPU can emit, indexed by identity.
///
/// Handed to a front end once so it can expand a frame itself — on a GPU, in
/// a shader — instead of being sent pixels. Keeping the table's construction
/// here is what stops the two ends disagreeing about a color.
pub fn palette_table() -> [u32; COLOR_IDS] {
    let mut out = [0; COLOR_IDS];
    let mut id = 0;
    while id < COLOR_IDS {
        out[id] = expand(id as u16);
        id += 1;
    }
    out
}

#[derive(Clone)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub struct Ppu {
    // ---- Register state ----
    ctrl: u8,
    mask: u8,
    status: u8,
    oam_addr: u8,
    /// Where the sprite unit is in secondary OAM.
    ///
    /// Evaluation fills it and the sprite fetch walks back over it, one byte
    /// every two dots. A full fetch of thirty-two bytes brings this back to
    /// zero by dot 321, which is why a `$2004` read taken after that normally
    /// sees index zero. Turn rendering off for part of the fetch and the
    /// stepping stops with it, leaving the address somewhere else.
    oam2_addr: u8,

    /// Whether the secondary OAM address has run off the end of it.
    ///
    /// This is the eight-sprite limit. Nothing counts sprites: the address
    /// advances once per byte copied, and when it wraps past the last of the
    /// thirty-two it latches here, which both stops any further copying and
    /// freezes the address where it stands. AccuracyCoin's page 22 can see
    /// the difference, because a `$2004` read late on a scanline reads
    /// through that same address.
    oam2_full: bool,
    /// The OAM address sprite evaluation began from.
    ///
    /// Evaluation walks OAM from wherever the address happens to point when it
    /// starts, which need not be zero and need not even be a multiple of four
    /// — a program can leave it anywhere. Held for the length of the scanline
    /// because the address register itself is cleared during sprite fetching.
    eval_start: u8,

    /// Loopy's registers: `v` live address, `t` latch, `x` fine scroll,
    /// `w` the shared first/second-write toggle.
    v: u16,
    t: u16,
    x: u8,
    w: bool,

    /// `$2007` reads of anything below the palette are delayed by one access.
    read_buffer: u8,

    // ---- Memory ----
    #[cfg_attr(feature = "snapshot", serde(with = "crate::snapshot::bytes"))]
    vram: [u8; 0x800],
    palette: [u8; 32],
    /// Whether A12 of the PPU address bus is high, and how many dots it has
    /// spent low if it is not. A cartridge that counts A12 edges is told only
    /// about the rises, so the dots in between cost it nothing.
    a12_level: bool,
    a12_low_dots: u16,
    /// Which of the two nametables each quadrant reads, as a byte offset into
    /// [`Ppu::vram`]. Taken from the cartridge when it is written to.
    nt_bank: [usize; 4],
    /// Each of the thirty-two palette entries as it finally reaches the
    /// picture: the mirrors resolved, greyscale applied, and the emphasis
    /// bits already above it. Built when the palette or `$2001` changes,
    /// which is a handful of times a frame, rather than at every pixel.
    emitted: [u16; 32],
    #[cfg_attr(feature = "snapshot", serde(with = "crate::snapshot::bytes"))]
    pub oam: [u8; 256],
    /// The eight sprites selected for the next scanline.
    secondary_oam: [u8; 32],

    // ---- Timing ----
    pub scanline: i16,
    pub dot: u16,
    pub frame: u64,
    odd_frame: bool,
    tv: TvSystem,
    /// Fractional PPU dots owed to the CPU clock, for PAL's 3.2:1 ratio.
    dot_accum: u16,

    // ---- Background pipeline ----
    nt_latch: u8,
    /// Pattern address for the tile being fetched, latched at the nametable
    /// read so later register writes cannot change it mid-fetch.
    tile_addr: u16,
    at_latch: u8,
    bg_lo_latch: u8,
    bg_hi_latch: u8,
    /// The four background shift registers in one word, sixteen bits each:
    /// the low half of the pattern at the bottom, then its high half, then
    /// the two attribute bits. They are only ever shifted together and read
    /// together, and one word is one shift and one load where four were.
    bg_shifters: u64,

    // ---- Sprite pipeline ----
    /// Sprite evaluation progress: OAM sprite index, byte index within it,
    /// and how many have been copied to secondary OAM so far.
    eval_n: usize,
    eval_m: usize,
    eval_done: bool,
    /// Each drawn sprite's shifter counter, loaded with its X position as the
    /// fetch reads it. The hardware counts it down a dot at a time and draws
    /// the sprite once it has run out; this holds the position it was loaded
    /// with and counts the dots in `sprite_elapsed` instead, which comes to
    /// the same thing and costs a great deal less.
    sprite_counter: [u8; 8],
    /// How many dots have been counted since the last of those positions was
    /// loaded. The counters are not stepped one at a time any more: they all
    /// count together, so one number serves all eight, and a sprite is due
    /// once this reaches its position.
    sprite_elapsed: u16,
    /// The nearest position among the sprites still on their way, so that a
    /// dot where none of them arrives costs one comparison. Set to
    /// `SPRITE_UNKNOWN` when the positions or the waiting set have changed and
    /// it has to be worked out again.
    sprite_soonest: u16,
    /// Whether each counter is counting rather than halted. A halted sprite is
    /// one being drawn; the end of a line sets them all counting again, but
    /// only if rendering is on to do it.
    sprite_counting: u8,
    /// Which sprites are being drawn *and* still have something to draw. Kept
    /// beside the counters so the drawing can skip the eight of them in one
    /// test, which on most dots it can.
    sprite_live: u8,
    /// The three bytes of a sprite the fetch reads before its pattern.
    sprite_y: [u8; 8],
    sprite_tile: [u8; 8],
    sprite_attr: [u8; 8],
    sprite_lo: [u8; 8],
    sprite_hi: [u8; 8],
    /// Pattern address for each slot, computed once the sprites are chosen.
    sprite_addr: [u16; 8],
    /// Whether sprite zero is among the sprites loaded for this scanline.
    sprite_zero_loaded: bool,
    sprite_zero_next: bool,

    /// Address held on the PPU bus. It persists between fetches, which is what
    /// lets an A12 counter time how long the line stayed low.
    bus_addr: u16,

    /// The low eight address lines, as held by the latch outside the PPU.
    ///
    /// Those lines are shared with the data lines, so the PPU only drives
    /// them for the first of an access's two dots and a latch holds them for
    /// the second. The upper lines are driven throughout. So anything that
    /// puts a different address out in between the two — a `$2006` write, a
    /// `$2007` access — changes the upper half of what gets read without
    /// changing the lower, and the read lands somewhere neither meant.
    ale_latch: u8,

    /// The last byte the PPU read off its bus.
    bus_data: u8,

    /// Dots left before a `$2007` read fills the read buffer.
    ///
    /// The buffer is not filled where the access happens but two dots later.
    /// While rendering, it takes whatever the fetch landing on that dot read:
    /// the register does not get a read of its own, it borrows the one the
    /// renderer was already doing.
    buffer_fill: u8,

    nmi_output: bool,
    /// Whether rendering is running. A `$2001` write reaches this one dot
    /// later and `prev_rendering_enabled` a dot after that; splits use it.
    rendering_enabled: bool,
    prev_rendering_enabled: bool,
    need_state_update: bool,

    /// Set when `$2002` is read on the dot before vblank begins. The flag
    /// loses the collision: no flag that frame, and no NMI.
    vbl_suppress: bool,
    /// Bit 7 as it stood when the current read of `$2002` began; see
    /// [`Ppu::begin_status_read`].
    vbl_latch: u8,
    /// A sprite zero hit found on the dot that just went by. The comparison
    /// is made as the pixel leaves; the flag behind it is set a dot later.
    zero_hit_pending: bool,
    /// The row of OAM that switching rendering off left to be overwritten.
    /// See [`Ppu::corrupt_oam`].
    oam_corrupt_row: Option<u8>,
    /// A write to one of the registers, waiting for the dot that acts on it.
    /// The processor lets go of the byte as `M2` falls; the PPU takes it in on
    /// its own next edge, so nothing a write does is felt on the dot it
    /// arrived on.
    /// Whether either of the two below is holding something. A dot asks this
    /// one flag rather than opening both boxes, and on all but a handful of
    /// dots a frame they are empty.
    pending_any: bool,
    pending_write: Option<(u16, u8, u8)>,
    /// A `$2007` read taken while rendering, and the dots left before the PPU
    /// acts on it. The processor's access does not reach the fetch cadence
    /// where it is made: it waits the same three dots the scroll counter
    /// takes, then puts its address out, and the byte follows two dots later.
    pending_fetch: Option<(u16, u8)>,
    /// A `$2007` access's step of the scroll counters, waiting the same way.
    pending_increment: Option<u8>,
    /// The byte the sprite unit has in hand. Everything it does passes through
    /// here — the clear, the copying, the fetch — and a `$2004` read taken
    /// while rendering finds this rather than OAM itself.
    oam_buffer: u8,
    /// A ninth sprite found on the line, on its way to the overflow flag: the
    /// flag is not raised on the step that finds it, but on the one after.
    eval_overflow_pending: bool,
    /// Steps still owed to the ninth sprite: having found it, the unit reads
    /// the rest of it out before giving up on the line entirely.
    eval_extra: u8,

    /// What the cartridge wants from the PPU's bus, asked once when it is
    /// opened. Almost every mapper wants nothing, and then the drawing never
    /// calls into it at all.
    mapper_watches_bus: bool,
    mapper_serves_nametables: bool,

    /// Value last seen on the PPU's own data bus, returned by write-only reads.
    open_bus: u8,
    /// The frame on which the data bus was last driven, for [`Ppu::decay_bus`].
    open_bus_frame: u64,

    /// The finished picture, as color identities rather than RGB: the
    /// six-bit palette entry in the low bits, the three `$2001` emphasis bits
    /// above it. See [`expand`].
    ///
    /// The only picture kept. Both front ends want something else anyway, so
    /// an RGB plane here would be a palette lookup and a second store per
    /// pixel for a picture nobody reads.
    ///
    /// Boxed to keep it out of the machine's state; save states skip it.
    #[cfg_attr(feature = "snapshot", serde(skip, default = "crate::snapshot::blank_ids"))]
    color_ids: Box<[u16; FRAME_WIDTH * FRAME_HEIGHT]>,
}

mod background;
mod clock;
mod registers;
mod render;
mod scroll;
mod sprites;

/// How long the PPU's data bus holds a value before it leaks away.
///
/// The bus is capacitance, not a register, so a value left alone fades rather
/// than persisting: roughly six hundred milliseconds on real silicon, varying
/// with the chip and its temperature. Counted in frames because that is the
/// coarsest clock that still lands well inside the second AccuracyCoin allows,
/// on both television systems.
///
/// Modeled as the whole bus going at once. Hardware loses it bit by bit at
/// rates no two chips agree on, which is not a thing worth reproducing.
const BUS_DECAY_FRAMES: u64 = 36;

impl Ppu {
    pub fn new(tv: TvSystem) -> Self {
        Ppu {
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            oam2_addr: 0,
            oam2_full: false,
            eval_start: 0,
            v: 0,
            t: 0,
            x: 0,
            w: false,
            read_buffer: 0,
            vram: [0; 0x800],
            palette: [0; 32],
            a12_level: false,
            a12_low_dots: 0,
            nt_bank: [0; 4],
            emitted: [0; 32],
            oam: [0; 256],
            secondary_oam: [0xff; 32],
            scanline: 0,
            dot: 0,
            frame: 0,
            odd_frame: false,
            tv,
            dot_accum: 0,
            nt_latch: 0,
            tile_addr: 0,
            at_latch: 0,
            bg_lo_latch: 0,
            bg_hi_latch: 0,
            bg_shifters: 0,
            eval_n: 0,
            eval_m: 0,
            eval_done: true,
            sprite_counter: [0; 8],
            sprite_elapsed: 0,
            sprite_soonest: SPRITE_UNKNOWN,
            sprite_counting: 0xff,
            sprite_live: 0,
            sprite_y: [0; 8],
            sprite_tile: [0; 8],
            sprite_attr: [0; 8],
            sprite_lo: [0; 8],
            sprite_hi: [0; 8],
            sprite_addr: [0; 8],
            sprite_zero_loaded: false,
            sprite_zero_next: false,
            bus_addr: 0,
            ale_latch: 0,
            bus_data: 0,
            buffer_fill: 0,
            rendering_enabled: false,
            prev_rendering_enabled: false,
            need_state_update: false,
            vbl_suppress: false,
            vbl_latch: 0,
            zero_hit_pending: false,
            oam_corrupt_row: None,
            pending_any: false,
            pending_write: None,
            pending_fetch: None,
            pending_increment: None,
            oam_buffer: 0xff,
            eval_overflow_pending: false,
            eval_extra: 0,
            nmi_output: false,
            mapper_watches_bus: false,
            mapper_serves_nametables: false,
            open_bus: 0,
            open_bus_frame: 0,
            color_ids: Box::new([0; FRAME_WIDTH * FRAME_HEIGHT]),
        }
    }

    pub fn reset(&mut self) {
        // Reset clears the control and mask registers and the write toggle,
        // but leaves VRAM, OAM and the palette alone.
        self.ctrl = 0;
        self.mask = 0;
        self.refresh_emitted();
        self.rendering_enabled = false;
        self.prev_rendering_enabled = false;
        self.need_state_update = false;
        self.w = false;
        self.read_buffer = 0;
        self.nmi_output = false;
        self.odd_frame = false;
        self.scanline = 0;
        self.dot = 0;
    }

    /// The completed frame as color identities; see the field.
    pub fn color_ids(&self) -> &[u16; FRAME_WIDTH * FRAME_HEIGHT] {
        &self.color_ids
    }

    /// The `/NMI` line: a level held while vblank and the enable bit coincide,
    /// so toggling `$2000` during vblank can fire a second interrupt.
    /// Told once, when the cartridge is opened.
    pub fn set_mapper_interest(&mut self, watches_bus: bool, serves_nametables: bool) {
        self.mapper_watches_bus = watches_bus;
        self.mapper_serves_nametables = serves_nametables;
    }

    pub fn nmi_line(&self) -> bool {
        self.nmi_output && (self.status & 0x80 != 0)
    }

    /// What the rendering pipeline acts on: the twice-delayed flag.
    fn rendering(&self) -> bool {
        self.prev_rendering_enabled
    }

    /// The register bit as written, with no delay. `$2007` accesses and the
    /// rendering-disabled palette quirk look at this rather than the pipeline.
    fn mask_rendering(&self) -> bool {
        self.mask & 0x18 != 0
    }

    /// Advance the delayed copies of the rendering-enable bit by one step.
    fn update_state(&mut self) {
        self.need_state_update = false;

        if self.prev_rendering_enabled != self.rendering_enabled {
            self.prev_rendering_enabled = self.rendering_enabled;
            if self.scanline < 240 && !self.prev_rendering_enabled {
                // Switching rendering off mid-screen hands the PPU bus back to
                // the current VRAM address.
                self.bus_addr = self.v & 0x3fff;
                // It also leaves the sprite unit halfway through something,
                // and a row of OAM pays for it when rendering comes back.
                if (1..=320).contains(&self.dot) {
                    self.oam_corrupt_row = Some(self.oam2_addr & 0x1f);
                }
                if (65..=256).contains(&self.dot) {
                    // Doing so during sprite evaluation nudges the OAM address.
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                }
            }
        }

        if self.rendering_enabled != self.mask_rendering() {
            self.rendering_enabled = self.mask_rendering();
            // One more step is needed before the pipeline sees it.
            self.need_state_update = true;
        }
    }

    fn last_scanline(&self) -> i16 {
        match self.tv {
            TvSystem::Ntsc => 260,
            TvSystem::Pal => 310,
            TvSystem::Dendy => 310,
        }
    }

    fn vblank_scanline(&self) -> i16 {
        match self.tv {
            TvSystem::Dendy => 291,
            _ => 241,
        }
    }
}
