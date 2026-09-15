# octet-emu

A cycle-accurate NES emulator, as a library. No shell, no window, no audio
device of its own: hand it a cartridge, run a frame, take the pixels and the
samples. The Android front end that drives it lives in
[octet-android](https://github.com/sky9t-sulia/octet-android).

## Using it

Two ways in, both over the same core:

- **C ABI** — `octet_emu_*`, for anything that can call C.
- **JNI** — `Emulator` in `io.github.sky9t_sulia.octet`, built by `cargo ndk`
  into `liboctet_emu.so`.

Open a cartridge and a handle comes back; pass it to everything else until it
is freed.

    cargo build --release            # the library
    cargo ndk -t arm64-v8a -o <dir> build --release --lib

## What it emulates

- 6502 CPU, undocumented opcodes included.
- PPU with per-cycle background, sprite and scroll timing.
- APU, mixed to signed 16-bit samples.
- NTSC and PAL.
- Save states, over `serde` and `postcard` — the whole machine, mapper state
  and battery memory with it.

Mappers: 0 (NROM), 1 (MMC1), 2 (UxROM), 3 (CNROM), 4 (MMC3, with 118 and 119),
5 (MMC5), 7 (AxROM), 9/10 (MMC2 and MMC4), 11 (Color Dreams),
16/159 (Bandai FCG, without its save EEPROM),
19 (Namco 163, without its wavetable voices),
21/22/23/25/27/183 (VRC2 and VRC4), 24/26 (VRC6, without its three voices), 28 (Action 53), 33/48 (Taito TC0190 and
TC0690), 34 (BNROM), 64 (RAMBO-1), 65 (Irem H3001), 66 (GxROM), 67/68
(Sunsoft-3 and Sunsoft-4), 69 (FME-7), 71 (Camerica and Codemasters),
73 (VRC3), 75 (VRC1), 88/95/154/206 (Namco 108 and its boards),
180 (UNROM 180), 232 (Camerica Quattro).

Anything else is refused by name rather than played wrong.

## Features

`snapshot` (on by default) brings in `serde` and `postcard` for save states.
Without it the emulation core carries no dependencies at all.

## License

MIT.
