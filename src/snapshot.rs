//! What a save state is made of, and the parts serde cannot describe by itself.
//!
//! Only built with the `snapshot` feature, so the bare core keeps its promise
//! of no dependencies.

use serde::de::{Error, SeqAccess, Visitor};
use serde::{Deserializer, Serializer};

/// A run of bytes longer than the 32 serde writes out element by element.
/// Sent as one blob instead, which postcard stores as a length and the bytes.
pub(crate) mod bytes {
    use super::*;

    pub fn serialize<S: Serializer, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[u8; N], D::Error> {
        struct Blob<const N: usize>;

        impl<'de, const N: usize> Visitor<'de> for Blob<N> {
            type Value = [u8; N];

            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "{N} bytes")
            }

            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                v.try_into().map_err(|_| E::invalid_length(v.len(), &self))
            }

            /// Formats that have no notion of a blob hand the bytes over one
            /// at a time instead.
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = [0u8; N];
                for (at, slot) in out.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| A::Error::invalid_length(at, &self))?;
                }
                Ok(out)
            }
        }

        d.deserialize_bytes(Blob::<N>)
    }
}

/// The picture is not state: it is what the last frame drew, and the next
/// frame draws it again. Left out of the file, and blank until then.
pub(crate) fn blank_frame() -> alloc::boxed::Box<[u32; crate::FRAME_WIDTH * crate::FRAME_HEIGHT]> {
    alloc::boxed::Box::new([0; crate::FRAME_WIDTH * crate::FRAME_HEIGHT])
}

/// The mapper, which is a trait object and so has no type serde can name.
///
/// Written as the bytes the board itself produced. Reading gives a
/// [`Pending`] holding those bytes: the real board is the one already on the
/// cartridge, and [`crate::Nes::load_state`] hands them to it.
pub(crate) mod mapper {
    use super::*;
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    // The reference to the Box is serde's to give: `with` is handed the
    // field as it stands, and the field is a Box.
    #[allow(clippy::borrowed_box)]
    pub fn serialize<S: Serializer>(
        m: &Box<dyn crate::mapper::Mapper>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&m.save_state())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Box<dyn crate::mapper::Mapper>, D::Error> {
        struct Blob;

        impl<'de> Visitor<'de> for Blob {
            type Value = Vec<u8>;

            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_str("a mapper's saved registers")
            }

            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                Ok(v.to_vec())
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = seq.next_element()? {
                    out.push(b);
                }
                Ok(out)
            }
        }

        Ok(Box::new(Pending(d.deserialize_bytes(Blob)?)))
    }
}

/// A mapper's saved bytes, waiting to be given to the real board. Never
/// reaches the bus: it lives only between reading a file and restoring it.
pub(crate) struct Pending(pub alloc::vec::Vec<u8>);

impl crate::mapper::Mapper for Pending {
    fn save_state(&self) -> alloc::vec::Vec<u8> {
        self.0.clone()
    }

    fn load_state(&mut self, data: &[u8]) -> bool {
        self.0 = data.to_vec();
        true
    }

    fn cpu_peek(&self, _addr: u16) -> Option<u8> {
        None
    }

    fn cpu_write(&mut self, _addr: u16, _val: u8) {}

    fn ppu_read(&mut self, _addr: u16) -> u8 {
        0
    }

    fn ppu_write(&mut self, _addr: u16, _val: u8) {}

    fn mirroring(&self) -> crate::Mirroring {
        crate::Mirroring::Horizontal
    }
}
