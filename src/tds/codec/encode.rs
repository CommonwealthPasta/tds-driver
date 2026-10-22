use super::{Packet, PacketCodec};
use asynchronous_codec::Encoder;
use bytes::{BufMut, BytesMut};

pub(crate) trait Encode<B: BufMut> {
    fn encode(self, dst: &mut B) -> crate::Result<()>;
}

/// A B_VARCHAR: `s` in UTF-16 after a u8 count of its units. `what` names
/// it in the error for a longer one.
pub(crate) fn put_b_varchar(dst: &mut BytesMut, s: &str, what: &str) -> crate::Result<()> {
    dst.put_u8(utf16_units(s, what)?);
    put_utf16(dst, s);
    Ok(())
}

/// A B_VARBYTE holding `s` in UTF-16: a u8 count of its bytes, two per unit.
/// `what` names it in the error for a longer one.
pub(crate) fn put_b_varbyte_utf16(dst: &mut BytesMut, s: &str, what: &str) -> crate::Result<()> {
    let units = s.encode_utf16().count();
    let len = u8::try_from(units * 2).map_err(|_| {
        crate::Error::Conversion(
            format!(
                "{what} is {units} UTF-16 code units long; the limit is {}",
                u8::MAX / 2
            )
            .into(),
        )
    })?;
    dst.put_u8(len);
    put_utf16(dst, s);
    Ok(())
}

/// A US_VARCHAR: as [`put_b_varchar`], with a u16 count.
pub(crate) fn put_us_varchar(dst: &mut BytesMut, s: &str, what: &str) -> crate::Result<()> {
    dst.put_u16_le(utf16_units(s, what)?);
    put_utf16(dst, s);
    Ok(())
}

fn utf16_units<T: TryFrom<usize> + num_traits::Bounded + std::fmt::Display>(
    s: &str,
    what: &str,
) -> crate::Result<T> {
    let units = s.encode_utf16().count();
    T::try_from(units).map_err(|_| {
        crate::Error::Conversion(
            format!(
                "{what} is {units} UTF-16 code units long; the limit is {}",
                T::max_value()
            )
            .into(),
        )
    })
}

fn put_utf16(dst: &mut BytesMut, s: &str) {
    for unit in s.encode_utf16() {
        dst.put_u16_le(unit);
    }
}

impl Encoder for PacketCodec {
    type Item<'a> = Packet;
    type Error = crate::Error;

    fn encode(&mut self, item: Packet, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.encode(dst)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tds::codec::{PacketHeader, PacketType};

    #[test]
    fn encode_writes_header_and_payload_to_dst() {
        let payload = BytesMut::from(&b"abcd"[..]);
        let packet = Packet::new(PacketHeader::batch(1), payload);

        let mut dst = BytesMut::new();
        let mut codec = PacketCodec;
        codec.encode(packet, &mut dst).expect("encode must succeed");

        // 8-byte header + 4-byte payload; a no-op encode would leave dst empty.
        assert_eq!(dst.len(), 12);
        assert_eq!(dst[0], PacketType::SQLBatch as u8);
        // Total length is patched into the BE length field (bytes 2..4).
        assert_eq!(&dst[2..4], &12u16.to_be_bytes());
        assert_eq!(&dst[8..], b"abcd");
    }
}
