use crate::{error::Error, sql_read_bytes::SqlReadBytes, tds::Numeric, ColumnData};
use bytes::BufMut;

/// Encode an `f64` as a money/smallmoney value into `dst`, prefixed with a
/// single length byte (as expected for a nullable `Money`/`Moneyn` column in a
/// bulk-load row). `max_len` is the column's declared length (8 for `money`,
/// 4 for `smallmoney`). Money is stored on the wire as a scaled integer
/// (value * 10_000).
pub(crate) fn encode<B>(dst: &mut B, max_len: usize, val: f64)
where
    B: BufMut,
{
    if max_len == 4 {
        dst.put_u8(4);
        dst.put_i32_le((val * 1e4).round() as i32);
    } else {
        dst.put_u8(8);
        let scaled = (val * 1e4).round() as i64;
        // money is transmitted as two 32-bit words, high word first.
        dst.put_i32_le((scaled >> 32) as i32);
        dst.put_u32_le(scaled as u32);
    }
}

/// Decodes to an exact scale-4 [`Numeric`], as SqlClient decodes to `decimal`;
/// an `f64` can't hold money's 19 digits.
pub(crate) async fn decode<R>(src: &mut R, len: u8) -> crate::Result<ColumnData<'static>>
where
    R: SqlReadBytes + Unpin,
{
    let scaled = match len {
        0 => return Ok(ColumnData::Numeric(None)),
        4 => src.read_i32_le().await? as i64,
        8 => {
            // Two 32-bit words, high word first.
            let high = src.read_i32_le().await? as i64;
            let low = src.read_u32_le().await? as i64;

            (high << 32) | low
        }
        _ => {
            return Err(Error::Protocol(
                format!("money: length of {} is invalid", len).into(),
            ))
        }
    };

    Ok(ColumnData::Numeric(Some(Numeric::new_with_scale(
        scaled as i128,
        4,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
    use bytes::{BufMut, BytesMut};

    async fn decode_scaled(len: u8, wire: &[u8]) -> ColumnData<'static> {
        let buf = BytesMut::from(wire);
        decode(&mut buf.into_sql_read_bytes(), len).await.unwrap()
    }

    fn money(scaled: i64) -> ColumnData<'static> {
        ColumnData::Numeric(Some(Numeric::new_with_scale(scaled as i128, 4)))
    }

    #[tokio::test]
    async fn decode_smallmoney_arm() {
        for v in [12345, i32::MIN, i32::MAX, -1] {
            assert_eq!(decode_scaled(4, &v.to_le_bytes()).await, money(v as i64));
        }
    }

    // Two 32-bit words, high word first; f64 rounded the low digits away.
    #[tokio::test]
    async fn decode_money_arm_is_exact() {
        for v in [
            (1i64 << 32) + 30000,
            i64::MAX,
            i64::MAX - 1,
            i64::MIN,
            -1,
            -(1i64 << 32),
        ] {
            let mut wire = ((v >> 32) as i32).to_le_bytes().to_vec();
            wire.extend_from_slice(&(v as u32).to_le_bytes());
            assert_eq!(decode_scaled(8, &wire).await, money(v), "{v}");
        }

        let max = decode_scaled(8, &[0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0xff]).await;
        assert_eq!(format!("{max:?}"), "Numeric(Some(922337203685477.5807))");
    }

    #[tokio::test]
    async fn decode_null() {
        assert_eq!(decode_scaled(0, &[]).await, ColumnData::Numeric(None));
    }

    // Reverses the on-wire money representation the same way `decode` does,
    // so we can assert `encode` is the exact inverse without a live server.
    fn decode_bytes(bytes: &[u8]) -> f64 {
        let len = bytes[0];
        match len {
            4 => i32::from_le_bytes(bytes[1..5].try_into().unwrap()) as f64 / 1e4,
            8 => {
                let high = i32::from_le_bytes(bytes[1..5].try_into().unwrap()) as i64;
                let low = u32::from_le_bytes(bytes[5..9].try_into().unwrap()) as f64;
                ((high << 32) as f64 + low) / 1e4
            }
            _ => panic!("invalid length"),
        }
    }

    #[test]
    fn encode_smallmoney_roundtrips() {
        let mut buf = Vec::new();
        encode(&mut buf, 4, 1234.5678);
        assert_eq!(buf[0], 4);
        assert_eq!(buf.len(), 5);
        assert_eq!(decode_bytes(&buf), 1234.5678);
    }

    // A length other than 0/4/8 is an invalid money encoding. Covers 38-42.
    #[tokio::test]
    async fn decode_invalid_length_errors() {
        let mut buf = BytesMut::new();
        buf.put_u8(0);
        let err = decode(&mut buf.into_sql_read_bytes(), 5).await.unwrap_err();
        assert!(matches!(err, Error::Protocol(_)), "got {err:?}");
    }

    // The `decode_bytes` test helper panics on an unexpected length prefix.
    // Covers line 99 (the helper's fallthrough arm).
    #[test]
    fn decode_bytes_invalid_length_panics() {
        let result = std::panic::catch_unwind(|| decode_bytes(&[2u8, 0, 0, 0, 0]));
        assert!(result.is_err());
    }

    #[test]
    fn encode_money_roundtrips() {
        for val in [0.0, 1.0, -1.0, 1234.5678, -9999.9999, 92233720368.5477] {
            let mut buf = Vec::new();
            encode(&mut buf, 8, val);
            assert_eq!(buf[0], 8);
            assert_eq!(buf.len(), 9);
            assert!((decode_bytes(&buf) - val).abs() < 1e-3, "val={}", val);
        }
    }
}
