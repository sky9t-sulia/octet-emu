//! The 2C02 picture processor.
//!
//! Modelled dot by dot. The scroll register layout, the shift-register
//! pipeline and the two-scanline-deep sprite evaluation are the real ones,
//! because the timing quirks games rely on — mid-frame scroll splits, sprite
//! zero hit, the pre-render dot skip — only fall out of that structure.

use alloc::boxed::Box;

use crate::cartridge::{Mirroring, TvSystem};
use crate::mapper::Mapper;

pub const FRAME_WIDTH: usize = 256;
pub const FRAME_HEIGHT: usize = 240;

const DOTS_PER_SCANLINE: u16 = 341;

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

#[derive(Clone)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub struct Ppu {
    // ---- Register state ----
    ctrl: u8,
    mask: u8,
    status: u8,
    oam_addr: u8,

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
    bg_shift_lo: u16,
    bg_shift_hi: u16,
    at_shift_lo: u16,
    at_shift_hi: u16,

    // ---- Sprite pipeline ----
    sprite_count: usize,
    /// Sprite evaluation progress: OAM sprite index, byte index within it,
    /// and how many have been copied to secondary OAM so far.
    eval_n: usize,
    eval_m: usize,
    eval_found: usize,
    eval_done: bool,
    sprite_x: [u8; 8],
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

    nmi_output: bool,
    /// Whether rendering is running. A `$2001` write reaches this one dot
    /// later and `prev_rendering_enabled` a dot after that; splits use it.
    rendering_enabled: bool,
    prev_rendering_enabled: bool,
    need_state_update: bool,

    /// Set when `$2002` is read on the dot before vblank begins. The flag
    /// loses the collision: no flag that frame, and no NMI.
    vbl_suppress: bool,

    /// Value last seen on the PPU's own data bus, returned by write-only reads.
    open_bus: u8,

    /// Boxed so the console's whole state does not carry a quarter-megabyte
    /// picture inline through every move; save states skip it entirely.
    #[cfg_attr(feature = "snapshot", serde(skip, default = "crate::snapshot::blank_frame"))]
    framebuffer: Box<[u32; FRAME_WIDTH * FRAME_HEIGHT]>,
}

mod background;
mod clock;
mod registers;
mod render;
mod scroll;
mod sprites;

impl Ppu {
    pub fn new(tv: TvSystem) -> Self {
        Ppu {
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            v: 0,
            t: 0,
            x: 0,
            w: false,
            read_buffer: 0,
            vram: [0; 0x800],
            palette: [0; 32],
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
            bg_shift_lo: 0,
            bg_shift_hi: 0,
            at_shift_lo: 0,
            at_shift_hi: 0,
            sprite_count: 0,
            eval_n: 0,
            eval_m: 0,
            eval_found: 0,
            eval_done: true,
            sprite_x: [0; 8],
            sprite_attr: [0; 8],
            sprite_lo: [0; 8],
            sprite_hi: [0; 8],
            sprite_addr: [0; 8],
            sprite_zero_loaded: false,
            sprite_zero_next: false,
            bus_addr: 0,
            rendering_enabled: false,
            prev_rendering_enabled: false,
            need_state_update: false,
            vbl_suppress: false,
            nmi_output: false,
            open_bus: 0,
            framebuffer: Box::new([0; FRAME_WIDTH * FRAME_HEIGHT]),
        }
    }

    pub fn reset(&mut self) {
        // Reset clears the control and mask registers and the write toggle,
        // but leaves VRAM, OAM and the palette alone.
        self.ctrl = 0;
        self.mask = 0;
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

    pub fn framebuffer(&self) -> &[u32; FRAME_WIDTH * FRAME_HEIGHT] {
        &self.framebuffer
    }

    /// The `/NMI` line: a level held while vblank and the enable bit coincide,
    /// so toggling `$2000` during vblank can fire a second interrupt.
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
