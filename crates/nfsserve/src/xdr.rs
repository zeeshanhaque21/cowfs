use std::io::{Read, Write};

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
pub type XDREndian = BigEndian;
use crate::nfs::nfsstring;

/// See <https://datatracker.ietf.org/doc/html/rfc1014>
#[allow(clippy::upper_case_acronyms)]
pub trait XDR {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()>;
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()>;
}

/// Serializes a basic enumeration.
/// Casts everything as u32 BigEndian
#[allow(non_camel_case_types)]
#[macro_export]
macro_rules! xdr_enum_serde {
    ($t:ident) => {
        impl XDR for $t {
            fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
                byteorder::WriteBytesExt::write_u32::<$crate::xdr::XDREndian>(dest, *self as u32)
            }
            fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
                let r: u32 = byteorder::ReadBytesExt::read_u32::<$crate::xdr::XDREndian>(src)?;
                if let Some(p) = num_traits::FromPrimitive::from_u32(r) {
                    *self = p;
                } else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Invalid value for {}", stringify!($t)),
                    ));
                }
                Ok(())
            }
        }
    };
}

/// Serializes a bool as a 4 byte big endian integer.
impl XDR for () {
    fn serialize<R: Write>(&self, _: &mut R) -> std::io::Result<()> {
        Ok(())
    }
    fn deserialize<R: Read>(&mut self, _: &mut R) -> std::io::Result<()> {
        Ok(())
    }
}

impl<A: XDR, B: XDR> XDR for (A, B) {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        self.0.serialize(dest)?;
        self.1.serialize(dest)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        self.0.deserialize(src)?;
        self.1.deserialize(src)
    }
}

impl XDR for bool {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        let val: u32 = *self as u32;
        dest.write_u32::<XDREndian>(val)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        let val: u32 = src.read_u32::<XDREndian>()?;
        *self = val > 0;
        Ok(())
    }
}

/// Serializes a i32 as a 4 byte big endian integer.
impl XDR for i32 {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        dest.write_i32::<XDREndian>(*self)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        *self = src.read_i32::<XDREndian>()?;
        Ok(())
    }
}

/// Serializes a i64 as a 8 byte big endian integer.
impl XDR for i64 {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        dest.write_i64::<XDREndian>(*self)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        *self = src.read_i64::<XDREndian>()?;
        Ok(())
    }
}

/// Serializes a u32 as a 4 byte big endian integer.
impl XDR for u32 {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        dest.write_u32::<XDREndian>(*self)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        *self = src.read_u32::<XDREndian>()?;
        Ok(())
    }
}

/// Serializes a u64 as a 8 byte big endian integer.
impl XDR for u64 {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        dest.write_u64::<XDREndian>(*self)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        *self = src.read_u64::<XDREndian>()?;
        Ok(())
    }
}

impl<const N: usize> XDR for [u8; N] {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        dest.write_all(self)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        src.read_exact(self)
    }
}

/// Largest opaque or array the decoder accepts, so a bad length cannot force a huge allocation.
pub const MAX_OPAQUE: u32 = 8 * 1024 * 1024;

fn too_long() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "XDR length out of range")
}

impl XDR for Vec<u8> {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        let length = u32::try_from(self.len()).map_err(|_| too_long())?;
        length.serialize(dest)?;
        dest.write_all(self)?;
        // write padding
        let pad = ((4 - length % 4) % 4) as usize;
        let zeros: [u8; 4] = [0, 0, 0, 0];
        if pad > 0 {
            dest.write_all(&zeros[..pad])?;
        }
        Ok(())
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        let mut length: u32 = 0;
        length.deserialize(src)?;
        if length > MAX_OPAQUE {
            return Err(too_long());
        }
        self.resize(length as usize, 0);
        src.read_exact(self)?;
        // read padding
        let pad = ((4 - length % 4) % 4) as usize;
        let mut zeros: [u8; 4] = [0, 0, 0, 0];
        src.read_exact(&mut zeros[..pad])?;
        Ok(())
    }
}

impl XDR for nfsstring {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        self.0.serialize(dest)
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        self.0.deserialize(src)
    }
}

impl XDR for Vec<u32> {
    fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
        let length = u32::try_from(self.len()).map_err(|_| too_long())?;
        length.serialize(dest)?;
        for i in self {
            i.serialize(dest)?;
        }
        Ok(())
    }
    fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
        let mut length: u32 = 0;
        length.deserialize(src)?;
        if length > MAX_OPAQUE / 4 {
            return Err(too_long());
        }
        self.resize(length as usize, 0);
        for i in self {
            i.deserialize(src)?;
        }
        Ok(())
    }
}

#[allow(non_camel_case_types)]
#[macro_export]
macro_rules! xdr_struct {
    (
        $t:ident,
        $($element:ident),*
    ) => {
        impl XDR for $t {
            fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
                $(self.$element.serialize(dest)?;)*
                Ok(())
            }
            fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
                $(self.$element.deserialize(src)?;)*
                Ok(())
            }
        }
    };
}

/// This macro only handles XDR Unions of the form
///       union pre_op_attr switch (bool attributes_follow) {
///       case TRUE:
///            wcc_attr  attributes;
///       case FALSE:
///            void;
///       };
/// This is translated to
///       enum pre_op_attr  {
///          Void,
///          attributes(wcc_attr)
///       }
/// The serde methods can be generated with XDRBoolUnion(pre_op_attr, attributes, wcc_attr)
/// The "true" type must have the Default trait
#[allow(non_camel_case_types)]
#[macro_export]
macro_rules! xdr_bool_union {
    (
        $t:ident, $enumcase:ident, $enumtype:ty
    ) => {
        impl XDR for $t {
            fn serialize<R: Write>(&self, dest: &mut R) -> std::io::Result<()> {
                match self {
                    $t::Void => {
                        false.serialize(dest)?;
                    }
                    $t::$enumcase(v) => {
                        true.serialize(dest)?;
                        v.serialize(dest)?;
                    }
                }
                Ok(())
            }
            fn deserialize<R: Read>(&mut self, src: &mut R) -> std::io::Result<()> {
                let mut c: bool = false;
                c.deserialize(src)?;
                if c == false {
                    *self = $t::Void;
                } else {
                    let mut r = <$enumtype>::default();
                    r.deserialize(src)?;
                    *self = $t::$enumcase(r);
                }
                Ok(())
            }
        }
    };
}

pub(crate) use xdr_bool_union;
pub(crate) use xdr_enum_serde;
pub(crate) use xdr_struct;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn opaque_round_trips_with_padding() {
        for len in 0..9usize {
            let v: Vec<u8> = (0..len as u8).collect();
            let mut buf = Vec::new();
            v.serialize(&mut buf).unwrap();
            assert_eq!(buf.len(), 4 + len.div_ceil(4) * 4);
            let mut back: Vec<u8> = Vec::new();
            back.deserialize(&mut Cursor::new(buf)).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn oversized_lengths_are_rejected_without_allocating() {
        let mut huge = Vec::new();
        u32::MAX.serialize(&mut huge).unwrap();
        assert!(Vec::<u8>::new()
            .deserialize(&mut Cursor::new(huge.clone()))
            .is_err());
        assert!(Vec::<u32>::new()
            .deserialize(&mut Cursor::new(huge))
            .is_err());
    }

    #[test]
    fn truncated_input_is_an_error() {
        let mut n = 0u64;
        assert!(n.deserialize(&mut Cursor::new(vec![0u8; 3])).is_err());
        let mut v = Vec::<u8>::new();
        assert!(v
            .deserialize(&mut Cursor::new(vec![0, 0, 0, 9, 1]))
            .is_err());
    }

    #[test]
    fn bool_union_round_trips() {
        let a = crate::nfs::post_op_attr::attributes(crate::nfs::fattr3::default());
        let mut buf = Vec::new();
        a.serialize(&mut buf).unwrap();
        let mut back = crate::nfs::post_op_attr::Void;
        back.deserialize(&mut Cursor::new(buf)).unwrap();
        assert!(matches!(back, crate::nfs::post_op_attr::attributes(_)));
        let mut buf = Vec::new();
        crate::nfs::post_op_attr::Void.serialize(&mut buf).unwrap();
        assert_eq!(buf, [0, 0, 0, 0]);
    }
}
