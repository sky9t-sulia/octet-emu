//! The emulator, callable from outside Rust: a cartridge in, a frame at a
//! time-out. Two layers — a plain C ABI (`octet_emu_*`, pointer-and-length,
//! usable from any language), and a thin JNI layer over the same handle for
//! Android specifically. Buttons arrive as a byte per port, in the order the
//! pad clocks them out.
//!
//! Every C function is safe to call with a null handle. Handles are not
//! thread-safe: one console belongs to one thread.

use alloc::vec::Vec;

use crate::{Button, Cartridge, Nes, TvSystem};

/// Picture size in pixels. The framebuffer is this many `u32`, row by row.
pub const WIDTH: u32 = crate::FRAME_WIDTH as u32;
pub const HEIGHT: u32 = crate::FRAME_HEIGHT as u32;

/// The rate sound comes out at before a cartridge is open.
pub const SAMPLE_RATE: u32 = crate::apu::SAMPLE_RATE;

/// Which timings to run at. `Auto` takes the cartridge at its word; the others
/// override what its header claims.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Region {
    #[default]
    Auto = 0,
    Ntsc = 1,
    Pal = 2,
}

impl Region {
    fn apply(self, header: TvSystem) -> TvSystem {
        match self {
            Region::Auto => header,
            Region::Ntsc => TvSystem::Ntsc,
            Region::Pal => TvSystem::Pal,
        }
    }

    /// The region for an integer as it crosses the interface; anything
    /// unrecognized is taken as `Auto`.
    pub fn from_u32(value: u32) -> Region {
        match value {
            1 => Region::Ntsc,
            2 => Region::Pal,
            _ => Region::Auto,
        }
    }
}

/// The buttons of one pad, in shift-register order. A set bit is a held
/// button. This is the whole of the input interface.
pub const A: u8 = 1 << 0;
pub const B: u8 = 1 << 1;
pub const SELECT: u8 = 1 << 2;
pub const START: u8 = 1 << 3;
pub const UP: u8 = 1 << 4;
pub const DOWN: u8 = 1 << 5;
pub const LEFT: u8 = 1 << 6;
pub const RIGHT: u8 = 1 << 7;

const BUTTONS: [Button; 8] = [
    Button::A,
    Button::B,
    Button::Select,
    Button::Start,
    Button::Up,
    Button::Down,
    Button::Left,
    Button::Right,
];

/// An open console. Opaque across the interface.
pub struct Emu {
    nes: Nes,
    audio: Vec<f32>,
    held: [u8; 2],
}

impl Emu {
    /// Open a cartridge. Returns `None` if the image is not a cartridge this
    /// emulator can run.
    pub fn open(rom: &[u8]) -> Option<Emu> {
        Emu::open_region(rom, Region::Auto)
    }

    /// Open a cartridge, running it at `region` whatever its header claims.
    pub fn open_region(rom: &[u8], region: Region) -> Option<Emu> {
        let mut cart = Cartridge::from_ines(rom).ok()?;
        cart.tv = region.apply(cart.tv);
        let nes = Nes::new(cart).ok()?;
        Some(Emu { nes, audio: Vec::new(), held: [0; 2] })
    }

    /// Set every button of one pad at once. Ports beyond the second are ignored.
    pub fn set_buttons(&mut self, port: usize, buttons: u8) {
        if port >= 2 {
            return;
        }
        // Only the changes are passed on, so a pad that has not moved costs
        // nothing and the console sees no redundant writes.
        let changed = self.held[port] ^ buttons;
        for (bit, button) in BUTTONS.iter().enumerate() {
            if changed & (1 << bit) != 0 {
                self.nes.set_button(port, *button, buttons & (1 << bit) != 0);
            }
        }
        self.held[port] = buttons;
    }

    /// Run until the next frame is complete, gathering the sound made along
    /// the way.
    pub fn run_frame(&mut self) {
        self.nes.run_frame();
        self.nes.drain_audio(&mut self.audio);
    }

    pub fn reset(&mut self) {
        self.nes.reset();
    }

    /// The finished picture, one `u32` per pixel as `0x00RRGGBB`.
    /// The frame as the PPU's own identities, packed for a texture: the
    /// palette entry in the first byte of each pixel, the emphasis in the
    /// second. A front end that expands on its graphics card wants this rather
    /// A quarter of the work of expanding it here, and the only form the
    /// television signal can be rebuilt from.
    pub fn write_cells(&self, out: &mut [u8]) -> Option<usize> {
        let ids = self.nes.color_ids();
        let needed = ids.len() * 4;
        if out.len() < needed {
            return None;
        }
        for (id, cell) in ids.iter().zip(out.chunks_exact_mut(4)) {
            cell[0] = (id & 0x3f) as u8;
            cell[1] = ((id >> 6) & 7) as u8;
            cell[2] = 0;
            cell[3] = 0xff;
        }
        Some(needed)
    }

    /// Every colour the PPU can emit, emphasis folded in, as the same packing.
    /// Handed over once so the card can look them up itself.
    pub fn palette(out: &mut [u8]) -> Option<usize> {
        let table = crate::palette_table();
        let needed = table.len() * 4;
        if out.len() < needed {
            return None;
        }
        for (colour, cell) in table.iter().zip(out.chunks_exact_mut(4)) {
            cell[0] = (colour >> 16) as u8;
            cell[1] = (colour >> 8) as u8;
            cell[2] = *colour as u8;
            cell[3] = 0xff;
        }
        Some(needed)
    }

    pub fn sample_rate(&self) -> u32 {
        self.nes.sample_rate()
    }

    /// How many pictures a second the cartridge runs at, in thousandths.
    pub fn frame_rate(&self) -> u32 {
        self.nes.frame_rate()
    }

    /// Take up to `out.len()` samples, returning how many were written. Sound
    /// left over stays queued for the next call.
    pub fn take_audio(&mut self, out: &mut [f32]) -> usize {
        let taken = self.audio.len().min(out.len());
        out[..taken].copy_from_slice(&self.audio[..taken]);
        self.audio.drain(..taken);
        taken
    }

    /// The cartridge's battery-backed memory, empty if it has none.
    pub fn save_data(&self) -> &[u8] {
        self.nes.save_data()
    }

    pub fn load_save_data(&mut self, data: &[u8]) {
        self.nes.load_save_data(data);
    }

    /// A save state: the whole machine, frozen. Unlike [`Emu::save_data`],
    /// this restores exactly where it was taken, not just the cartridge's own
    /// save file.
    pub fn save_state(&self) -> Vec<u8> {
        self.nes.save_state()
    }

    /// Put the machine back where a save state left it. `false` if `data` was
    /// not a save state this cartridge can use.
    pub fn load_state(&mut self, data: &[u8]) -> bool {
        self.nes.load_state(data).is_ok()
    }
}

// ---- The C interface. Each function is the method above with the pointer
// checks a caller in another language needs.

/// Open a cartridge from `len` bytes at `rom`. Returns null if the image is
/// not one this emulator can run. Free it with [`octet_emu_free`].
///
/// # Safety
/// `rom` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_open(rom: *const u8, len: usize) -> *mut Emu {
    // Region 0 is `Region::Auto`, which is what opening without one means.
    unsafe { octet_emu_open_region(rom, len, 0) }
}

/// As [`octet_emu_open`], but running at `region`: 0 as the cartridge asks,
/// 1 NTSC, 2 PAL.
///
/// # Safety
/// `rom` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_open_region(rom: *const u8, len: usize, region: u32) -> *mut Emu {
    if rom.is_null() || len == 0 {
        return core::ptr::null_mut();
    }
    let bytes = unsafe { core::slice::from_raw_parts(rom, len) };
    match Emu::open_region(bytes, Region::from_u32(region)) {
        Some(emu) => Box::into_raw(Box::new(emu)),
        None => core::ptr::null_mut(),
    }
}

/// # Safety
/// `emu` must come from [`octet_emu_open`] and must not be used again.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_free(emu: *mut Emu) {
    if !emu.is_null() {
        drop(unsafe { Box::from_raw(emu) });
    }
}

/// # Safety
/// `emu` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_reset(emu: *mut Emu) {
    if let Some(emu) = unsafe { emu.as_mut() } {
        emu.reset();
    }
}

/// Set every button of `port` at once; see the button constants.
///
/// # Safety
/// `emu` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_set_buttons(emu: *mut Emu, port: u32, buttons: u8) {
    if let Some(emu) = unsafe { emu.as_mut() } {
        emu.set_buttons(port as usize, buttons);
    }
}

/// # Safety
/// `emu` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_run_frame(emu: *mut Emu) {
    if let Some(emu) = unsafe { emu.as_mut() } {
        emu.run_frame();
    }
}

/// The picture as the PPU's own identities, [`octet_emu_width`] by
/// [`octet_emu_height`] of them, four bytes each: the palette entry, the
/// emphasis, a spare, and an opaque alpha. Nothing here decides what colour an
/// identity is — [`octet_emu_palette`] hands over the table for that, and a
/// front end that wants what a television did rebuilds the signal instead.
///
/// # Safety
/// `emu` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_cells(emu: *const Emu, out: *mut u8, cap: usize) -> usize {
    let Some(emu) = (unsafe { emu.as_ref() }) else { return 0 };
    if out.is_null() {
        return 0;
    }
    let dst = unsafe { core::slice::from_raw_parts_mut(out, cap) };
    emu.write_cells(dst).unwrap_or(0)
}

/// Every colour the PPU can emit, 512 of them, four bytes each. Asked for once.
///
/// # Safety
/// `out` must point to `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_palette(out: *mut u8, cap: usize) -> usize {
    if out.is_null() {
        return 0;
    }
    let dst = unsafe { core::slice::from_raw_parts_mut(out, cap) };
    Emu::palette(dst).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn octet_emu_width() -> u32 {
    WIDTH
}

#[no_mangle]
pub extern "C" fn octet_emu_height() -> u32 {
    HEIGHT
}

/// # Safety
/// `emu` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_sample_rate(emu: *const Emu) -> u32 {
    unsafe { emu.as_ref() }.map_or(0, Emu::sample_rate)
}

/// Pictures a second, in thousandths: 60098 for one region, 50007 for the
/// other. A platform that runs every cartridge at the same rate plays half of
/// them a fifth too fast.
///
/// # Safety
/// `emu` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_frame_rate(emu: *const Emu) -> u32 {
    unsafe { emu.as_ref() }.map_or(60_098, Emu::frame_rate)
}

/// Take up to `cap` samples into `out`, returning how many were written.
///
/// # Safety
/// `emu` must be null or a live handle, and `out` must point to `cap`
/// writable floats.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_take_audio(emu: *mut Emu, out: *mut f32, cap: usize) -> usize {
    let Some(emu) = (unsafe { emu.as_mut() }) else { return 0 };
    if out.is_null() || cap == 0 {
        return 0;
    }
    emu.take_audio(unsafe { core::slice::from_raw_parts_mut(out, cap) })
}

/// Copy the battery-backed memory into `out`, returning how many bytes were
/// written.
///
/// # Safety
/// `emu` must be null or a live handle, and `out` must point to `cap`
/// writable bytes.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_save_read(emu: *const Emu, out: *mut u8, cap: usize) -> usize {
    let Some(emu) = (unsafe { emu.as_ref() }) else { return 0 };
    if out.is_null() {
        return 0;
    }
    let data = emu.save_data();
    let n = data.len().min(cap);
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), out, n) };
    n
}

/// Restore battery-backed memory saved earlier.
///
/// # Safety
/// `emu` must be null or a live handle, and `data` must point to `len`
/// readable bytes.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_save_write(emu: *mut Emu, data: *const u8, len: usize) {
    let Some(emu) = (unsafe { emu.as_mut() }) else { return };
    if data.is_null() || len == 0 {
        return;
    }
    emu.load_save_data(unsafe { core::slice::from_raw_parts(data, len) });
}

/// How many bytes a save state taken right now would be.
///
/// # Safety
/// `emu` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_state_len(emu: *const Emu) -> usize {
    unsafe { emu.as_ref() }.map_or(0, |emu| emu.save_state().len())
}

/// Take a save state right now, into `out`. Returns how many bytes were
/// written.
///
/// # Safety
/// `emu` must be null or a live handle, and `out` must point to `cap`
/// writable bytes.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_state_read(emu: *const Emu, out: *mut u8, cap: usize) -> usize {
    let Some(emu) = (unsafe { emu.as_ref() }) else { return 0 };
    if out.is_null() {
        return 0;
    }
    let data = emu.save_state();
    let n = data.len().min(cap);
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), out, n) };
    n
}

/// Restore a save state taken earlier. `false` if `data` was not one this
/// cartridge can use.
///
/// # Safety
/// `emu` must be null or a live handle, and `data` must point to `len`
/// readable bytes.
#[no_mangle]
pub unsafe extern "C" fn octet_emu_state_write(emu: *mut Emu, data: *const u8, len: usize) -> bool {
    let Some(emu) = (unsafe { emu.as_mut() }) else { return false };
    if data.is_null() || len == 0 {
        return false;
    }
    emu.load_state(unsafe { core::slice::from_raw_parts(data, len) })
}

// ---- JNI: the same handle, called from Kotlin as
// `io.github.sky9t_sulia.octet.Emulator`. Each function mirrors one of the
// plain C ones above, translating Java types at the boundary rather than
// duplicating any emulator logic.

#[cfg(target_os = "android")]
mod jni_bridge {
    use super::{Emu, Region};
    use jni::objects::{JByteBuffer, JClass};
    use jni::sys::{jboolean, jbyteArray, jint, jlong};
    use jni::{Env, EnvUnowned};

    fn handle_mut<'a>(handle: jlong) -> Option<&'a mut Emu> {
        unsafe { (handle as *mut Emu).as_mut() }
    }

    fn handle_ref<'a>(handle: jlong) -> Option<&'a Emu> {
        unsafe { (handle as *const Emu).as_ref() }
    }

    /// A native method is handed an `EnvUnowned`, which is only the pointer;
    /// the API lives on `Env`. Borrowing one also wraps the body in a
    /// `catch_unwind`, since a panic crossing back into the JVM aborts the
    /// process. `fallback` is what Java sees if that happens, or if the
    /// thread cannot be attached.
    fn in_env<'local, T>(
        env: &mut EnvUnowned<'local>,
        fallback: T,
        body: impl FnOnce(&mut Env<'local>) -> T,
    ) -> T {
        let outcome = env.with_env(|env| Ok::<T, jni::errors::Error>(body(env)));
        match outcome.into_outcome() {
            jni::Outcome::Ok(value) => value,
            _ => fallback,
        }
    }

    /// Hand bytes back to Java as a `byte[]`, or null if one cannot be made.
    /// Java has no unsigned byte, so the copy is reread as signed on the way
    /// out; both readers below want the same thing.
    fn byte_array(env: &mut Env<'_>, data: &[u8]) -> jbyteArray {
        let signed: alloc::vec::Vec<i8> = data.iter().map(|&b| b as i8).collect();
        match env.new_byte_array(signed.len()) {
            Ok(arr) => {
                let _ = arr.set_region(env, 0, &signed);
                arr.into_raw()
            }
            Err(_) => core::ptr::null_mut(),
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_openRegion<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        rom: jbyteArray,
        region: jint,
    ) -> jlong {
        in_env(&mut env, 0, |env| {
            let rom = unsafe { jni::objects::JByteArray::from_raw(env, rom) };
            let Ok(bytes) = env.convert_byte_array(&rom) else { return 0 };
            match Emu::open_region(&bytes, Region::from_u32(region as u32)) {
                Some(emu) => alloc::boxed::Box::into_raw(alloc::boxed::Box::new(emu)) as jlong,
                None => 0,
            }
        })
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_free<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
    ) {
        if handle != 0 {
            drop(unsafe { alloc::boxed::Box::from_raw(handle as *mut Emu) });
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_reset<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
    ) {
        if let Some(emu) = handle_mut(handle) {
            emu.reset();
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_setButtons<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
        port: jint,
        buttons: jint,
    ) {
        if let Some(emu) = handle_mut(handle) {
            emu.set_buttons(port as usize, buttons as u8);
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_runFrame<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
    ) {
        if let Some(emu) = handle_mut(handle) {
            emu.run_frame();
        }
    }

    /// Fills `out` (a direct `ByteBuffer` of `width*height*4` bytes) with the
    /// picture as the PPU's own identities. Nothing here says what colour one
    /// is: the card does that, from the table [`Emu::palette`] hands over.
    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_cells<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
        out: JByteBuffer<'local>,
    ) {
        in_env(&mut env, (), |env| {
            let Some(emu) = handle_ref(handle) else { return };
            let Ok(ptr) = env.get_direct_buffer_address(&out) else { return };
            let Ok(cap) = env.get_direct_buffer_capacity(&out) else { return };
            let dst = unsafe { core::slice::from_raw_parts_mut(ptr, cap) };
            emu.write_cells(dst);
        })
    }

    /// The palette, asked for once when the screen is set up.
    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_palette<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        out: JByteBuffer<'local>,
    ) {
        in_env(&mut env, (), |env| {
            let Ok(ptr) = env.get_direct_buffer_address(&out) else { return };
            let Ok(cap) = env.get_direct_buffer_capacity(&out) else { return };
            let dst = unsafe { core::slice::from_raw_parts_mut(ptr, cap) };
            Emu::palette(dst);
        })
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_width<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
    ) -> jint {
        super::WIDTH as jint
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_height<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
    ) -> jint {
        super::HEIGHT as jint
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_sampleRate<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
    ) -> jint {
        handle_ref(handle).map_or(0, Emu::sample_rate) as jint
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_frameRate<'local>(
        _env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
    ) -> jint {
        handle_ref(handle).map_or(60_098, Emu::frame_rate) as jint
    }

    /// Takes up to `out`'s own sample count (its byte capacity / 4) into it,
    /// as native-order `f32`s — the same layout `AudioTrack`'s
    /// `ENCODING_PCM_FLOAT` reads on the Kotlin side, so this is a straight
    /// copy, not a conversion. Returns how many samples were written.
    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_takeAudio<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
        out: JByteBuffer<'local>,
    ) -> jint {
        in_env(&mut env, 0, |env| {
            let Some(emu) = handle_mut(handle) else { return 0 };
            let Ok(ptr) = env.get_direct_buffer_address(&out) else { return 0 };
            let Ok(cap) = env.get_direct_buffer_capacity(&out) else { return 0 };
            let samples = cap / 4;
            let dst = unsafe { core::slice::from_raw_parts_mut(ptr as *mut f32, samples) };
            emu.take_audio(dst) as jint
        })
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_saveRead<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
    ) -> jbyteArray {
        in_env(&mut env, core::ptr::null_mut(), |env| {
            let data: &[u8] = handle_ref(handle).map_or(&[], |emu| emu.save_data());
            byte_array(env, data)
        })
    }

    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_saveWrite<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
        data: jbyteArray,
    ) {
        in_env(&mut env, (), |env| {
            let Some(emu) = handle_mut(handle) else { return };
            let arr = unsafe { jni::objects::JByteArray::from_raw(env, data) };
            if let Ok(bytes) = env.convert_byte_array(&arr) {
                emu.load_save_data(&bytes);
            }
        })
    }

    /// A save state, taken right now: the whole machine, not just the
    /// cartridge's own save file.
    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_stateSave<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
    ) -> jbyteArray {
        in_env(&mut env, core::ptr::null_mut(), |env| {
            let data = handle_ref(handle).map_or(alloc::vec::Vec::new(), |emu| emu.save_state());
            byte_array(env, &data)
        })
    }

    /// Restore a save state taken earlier. `false` if it was not one this
    /// cartridge can use.
    #[no_mangle]
    pub extern "system" fn Java_io_github_sky9t_1sulia_octet_Emulator_stateLoad<'local>(
        mut env: EnvUnowned<'local>,
        _class: JClass<'local>,
        handle: jlong,
        data: jbyteArray,
    ) -> jboolean {
        // `jboolean` is a real `bool` as of jni 0.22, not a byte.
        in_env(&mut env, false, |env| {
            let Some(emu) = handle_mut(handle) else { return false };
            let arr = unsafe { jni::objects::JByteArray::from_raw(env, data) };
            match env.convert_byte_array(&arr) {
                Ok(bytes) => emu.load_state(&bytes),
                Err(_) => false,
            }
        })
    }
}
