//! iNES / NES 2.0 ROM image parsing.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

/// Nametable arrangement requested by the cartridge at power-on. Mappers with
/// runtime-switchable mirroring override this; it is only the initial state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub enum Mirroring {
    Horizontal,
    Vertical,
    SingleScreenA,
    SingleScreenB,
    FourScreen,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "snapshot", derive(serde::Serialize, serde::Deserialize))]
pub enum TvSystem {
    Ntsc,
    Pal,
    Dendy,
}

#[derive(Debug)]
pub enum RomError {
    NotInes,
    Truncated,
}

pub struct Cartridge {
    pub prg_rom: Vec<u8>,
    pub chr_rom: Vec<u8>,
    /// Empty when the cartridge uses CHR ROM.
    pub chr_ram: Vec<u8>,
    pub prg_ram: Vec<u8>,
    /// Battery-backed portion of PRG RAM, saved between sessions.
    pub prg_nvram: Vec<u8>,
    pub mapper: u16,
    pub submapper: u8,
    pub mirroring: Mirroring,
    pub tv: TvSystem,
}

impl Cartridge {
    #[rustfmt::skip]
    pub fn from_ines(data: &[u8]) -> Result<Self, RomError> {
        if data.len() < 16 || &data[0..4] != b"NES\x1a" {
            return Err(RomError::NotInes);
        }
        // A NES 2.0 image sets bits 3..2 of byte 7 to 0b10.
        let nes2 = (data[7] & 0x0c) == 0x08;

        let mut prg_units = data[4] as usize;
        let mut chr_units = data[5] as usize;
        let mut mapper = ((data[6] >> 4) as u16) | ((data[7] & 0xf0) as u16);
        let mut submapper = 0u8;

        if nes2 {
            mapper |= ((data[8] & 0x0f) as u16) << 8;
            submapper = data[8] >> 4;
            // Byte 9 extends the size fields by four bits each. The value 0xf
            // switches that unit to an exponent-multiplier encoding instead.
            let hi_prg = (data[9] & 0x0f) as usize;
            let hi_chr = (data[9] >> 4) as usize;
            prg_units =
                if hi_prg == 0x0f { exponent_size(data[4]) } else { (hi_prg << 8) | prg_units };
            chr_units =
                if hi_chr == 0x0f { exponent_size(data[5]) } else { (hi_chr << 8) | chr_units };
        }

        let prg_len = if data[9..].is_empty() || !nes2 || (data[9] & 0x0f) != 0x0f {
            prg_units * 16384
        } else {
            prg_units
        };
        let chr_len = if !nes2 || (data[9] >> 4) != 0x0f { chr_units * 8192 } else { chr_units };

        let trainer = (data[6] & 0x04) != 0;
        let mut off = 16 + if trainer { 512 } else { 0 };
        if data.len() < off + prg_len + chr_len {
            return Err(RomError::Truncated);
        }

        let prg_rom = data[off..off + prg_len].to_vec();
        off += prg_len;
        let chr_rom = data[off..off + chr_len].to_vec();

        let four_screen = (data[6] & 0x08) != 0;
        let mirroring = if four_screen {
            Mirroring::FourScreen
        } else if (data[6] & 0x01) != 0 { Mirroring::Vertical } else { Mirroring::Horizontal };
        let battery = (data[6] & 0x02) != 0;

        // NES 2.0 states RAM sizes explicitly; iNES 1.0 does not, so assume the
        // common 8 KiB and let the mapper narrow it if it knows better.
        let (prg_ram, prg_nvram) = if nes2 {
            (vec![0; shift_size(data[10] & 0x0f)], vec![0; shift_size(data[10] >> 4)])
        } else if battery { (Vec::new(), vec![0; 8192]) } else { (vec![0; 8192], Vec::new()) };

        let chr_ram_len = if !chr_rom.is_empty() {
            0
        } else if nes2 {
            let n = shift_size(data[11] & 0x0f) + shift_size(data[11] >> 4);
            if n == 0 { 8192 } else { n }
        } else {
            8192
        };

        let tv = if nes2 {
            match data[12] & 0x03 {
                1 => TvSystem::Pal,
                2 | 3 => TvSystem::Ntsc, // multi-region images run as NTSC
                _ => TvSystem::Ntsc,
            }
        } else if (data[9] & 0x01) != 0 { TvSystem::Pal } else { TvSystem::Ntsc };

        Ok(Cartridge {
            prg_rom,
            chr_rom,
            chr_ram: vec![0; chr_ram_len],
            prg_ram,
            prg_nvram,
            mapper,
            submapper,
            mirroring,
            tv,
        })
    }
}

/// NES 2.0 exponent-multiplier form: size = 2^exponent * (multiplier*2 + 1).
fn exponent_size(byte: u8) -> usize {
    let exponent = (byte >> 2) as u32;
    let multiplier = (byte & 0x03) as usize * 2 + 1;
    (1usize << exponent.min(48)) * multiplier
}

/// NES 2.0 shift-count RAM size: 0 means none, otherwise 64 << n bytes.
#[rustfmt::skip]
fn shift_size(nibble: u8) -> usize {
    if nibble == 0 { 0 } else { 64 << nibble }
}
