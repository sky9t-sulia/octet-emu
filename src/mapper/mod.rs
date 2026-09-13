//! Mapper interface and the banking machinery shared by nearly all of them.

extern crate alloc;
use alloc::boxed::Box;

use crate::cartridge::{Cartridge, Mirroring};

mod action53;
mod axrom;
mod banks;
mod bf9096;
mod bf909x;
mod bnrom;
mod cnrom;
mod colordreams;
mod dxrom;
mod fme7;
mod gxrom;
mod h3001;
mod mmc1;
mod mmc2;
mod mmc3;
mod mmc5;
mod nrom;
mod rambo1;
mod sunsoft3;
mod sunsoft4;
mod taito;
mod unrom180;
mod uxrom;
mod vrc1;
mod vrc3;
mod vrc2_4;

pub use banks::Banks;

/// A cartridge mapper. Address ranges outside a method's documented window are
/// never passed to it, so implementations need not re-check them.
/// `Send` so a whole [`crate::Nes`] can be handed to a thread of its own.
///
/// Every board here is plain data, so this costs nothing to satisfy — but
/// without it the trait object poisons the machine that holds it, and a front
/// end cannot run the console anywhere but the thread that built it.
pub trait Mapper: Send {
    /// This board's registers, as bytes for a save state. Required rather
    /// than defaulted: a board that forgot would drop its banking into a file
    /// that loads with the wrong pages showing, rather than fail to build.
    #[cfg(feature = "snapshot")]
    fn save_state(&self) -> Vec<u8>;

    /// Put back what [`Mapper::save_state`] wrote. False if the bytes are not
    /// this board's.
    #[cfg(feature = "snapshot")]
    fn load_state(&mut self, data: &[u8]) -> bool;

    /// CPU read of `$4020...=$FFFF`. `None` leaves the data bus floating, which
    /// the bus resolves to the last value it drove (open bus).
    fn cpu_read(&mut self, addr: u16) -> Option<u8> {
        self.cpu_peek(addr)
    }

    /// Side-effect-free form of [`Mapper::cpu_read`], for debuggers and tracing.
    fn cpu_peek(&self, addr: u16) -> Option<u8>;

    /// CPU write to `$4020..=$FFFF`.
    fn cpu_write(&mut self, addr: u16, val: u8);

    /// PPU read of `$0000...=$1FFF` (pattern tables).
    fn ppu_read(&mut self, addr: u16) -> u8;

    /// PPU write to `$0000...=$1FFF`. A CHR ROM cartridge ignores it.
    fn ppu_write(&mut self, addr: u16, val: u8);

    /// Current nametable arrangement. May change at runtime.
    fn mirroring(&self) -> Mirroring;

    /// Mappers that supply their own nametable memory (MMC5, Sunsoft FME-7
    /// four-screen, …) override these to intercept `$2000...=$3EFF`.
    fn read_nametable(&mut self, _addr: u16) -> Option<u8> {
        None
    }

    fn write_nametable(&mut self, _addr: u16, _val: u8) -> bool {
        false
    }

    /// Level of the mapper's IRQ line, sampled by the CPU each cycle.
    fn irq(&self) -> bool {
        false
    }

    /// Whether this mapper wants [`Mapper::tick_cpu`] at all. Asked once, when
    /// the cartridge is opened.
    fn ticks_cpu(&self) -> bool {
        false
    }

    /// Called once per CPU cycle, for mappers whose IRQ counters run off the
    /// CPU clock (MMC5, the VRC family, FME-7).
    fn tick_cpu(&mut self) {}

    /// Whether this mapper listens to the PPU's own bus at all — the two
    /// calls below. Most do not, and the PPU is told so once when the
    /// cartridge is opened rather than asked a call a dot.
    fn watches_ppu_bus(&self) -> bool {
        false
    }

    /// Whether it answers for any of `$2000...=$3EFF` itself.
    fn serves_nametables(&self) -> bool {
        false
    }

    /// Called once per PPU dot with whatever address the PPU bus is holding,
    /// so A12-edge counters (MMC3) can measure how long the line stayed low.
    fn tick_ppu_addr(&mut self, _addr: u16) {}

    /// Called for each real PPU bus read, unlike [`Mapper::tick_ppu_addr`]:
    /// the MMC5 infers its position from the pattern of reads.
    fn ppu_bus_read(&mut self, _addr: u16) {}

    /// Called when the CPU writes one of `$2000...=$2007`. The MMC5 watches
    /// the sprite size and rendering bits to decide how to bank CHR.
    fn ppu_register_write(&mut self, _addr: u16, _val: u8) {}

    /// Battery-backed memory, for save-file persistence. Empty when none.
    fn nvram(&self) -> &[u8] {
        &[]
    }

    fn nvram_mut(&mut self) -> &mut [u8] {
        &mut []
    }
}

/// A write to ROM on an unbuffered board: CPU and ROM drive the bus together,
/// so the value that lands is the AND of the two.
pub(crate) fn bus_conflict(banks: &Banks, addr: u16, val: u8) -> u8 {
    val & banks.read_cpu(addr).unwrap_or(0xff)
}

#[derive(Debug)]
pub struct UnsupportedMapper(pub u16);

pub fn create(cart: Cartridge) -> Result<Box<dyn Mapper>, UnsupportedMapper> {
    let id = cart.mapper;
    match id {
        0 => Ok(Box::new(nrom::Nrom::new(cart))),
        1 => Ok(Box::new(mmc1::Mmc1::new(cart))),
        2 => Ok(Box::new(uxrom::UxRom::new(cart))),
        3 => Ok(Box::new(cnrom::CnRom::new(cart))),
        4 | 118 | 119 => {
            let sub = cart.submapper;
            Ok(Box::new(mmc3::Mmc3::new(cart, id, sub)))
        }
        5 => Ok(Box::new(mmc5::Mmc5::new(cart))),
        9 | 10 => Ok(Box::new(mmc2::Mmc2::new(cart, id))),
        7 => Ok(Box::new(axrom::AxRom::new(cart))),
        11 => Ok(Box::new(colordreams::ColorDreams::new(cart))),
        21 | 22 | 23 | 25 | 27 | 183 => {
            let sub = cart.submapper;
            Ok(Box::new(vrc2_4::Vrc2_4::new(cart, id, sub)))
        }
        28 => Ok(Box::new(action53::Action53::new(cart))),
        33 | 48 => Ok(Box::new(taito::Taito::new(cart, id))),
        34 => Ok(Box::new(bnrom::BnRom::new(cart))),
        64 => Ok(Box::new(rambo1::Rambo1::new(cart))),
        65 => Ok(Box::new(h3001::H3001::new(cart))),
        66 => Ok(Box::new(gxrom::GxRom::new(cart))),
        67 => Ok(Box::new(sunsoft3::Sunsoft3::new(cart))),
        68 => Ok(Box::new(sunsoft4::Sunsoft4::new(cart))),
        69 => Ok(Box::new(fme7::Fme7::new(cart))),
        73 => Ok(Box::new(vrc3::Vrc3::new(cart))),
        75 => Ok(Box::new(vrc1::Vrc1::new(cart))),
        71 => Ok(Box::new(bf909x::Bf909x::new(cart))),
        180 => Ok(Box::new(unrom180::UnRom180::new(cart))),
        88 | 95 | 154 | 206 => Ok(Box::new(dxrom::DxRom::new(cart, id))),
        232 => {
            let sub = cart.submapper;
            Ok(Box::new(bf9096::Bf9096::new(cart, sub)))
        }
        _ => Err(UnsupportedMapper(id)),
    }
}

/// The two state methods for a board that derives serde, which is every one of
/// them: written out once here rather than fifteen times.
#[cfg(feature = "snapshot")]
macro_rules! state_via_serde {
    () => {
        fn save_state(&self) -> alloc::vec::Vec<u8> {
            postcard::to_allocvec(self).unwrap_or_default()
        }

        fn load_state(&mut self, data: &[u8]) -> bool {
            let Ok(mut loaded) = postcard::from_bytes::<Self>(data) else {
                return false;
            };
            loaded.banks.keep_rom_of(&mut self.banks);
            *self = loaded;
            true
        }
    };
}

#[cfg(feature = "snapshot")]
pub(crate) use state_via_serde;
