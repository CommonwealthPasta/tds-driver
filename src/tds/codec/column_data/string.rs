use std::borrow::Cow;

use crate::{error::Error, sql_read_bytes::SqlReadBytes, tds::Collation, VarLenType};

pub(crate) async fn decode<R>(
    src: &mut R,
    ty: VarLenType,
    len: usize,
    collation: Option<Collation>,
) -> crate::Result<Option<Cow<'static, str>>>
where
    R: SqlReadBytes + Unpin,
{
    use VarLenType::*;

    let data = super::plp::decode(src, len).await?;

    match (data, ty) {
        // Codepages other than UTF
        (Some(buf), BigChar) | (Some(buf), BigVarChar) => {
            let collation = collation.as_ref().unwrap();
            Ok(Some(decode_varchar(buf, collation)?.into()))
        }
        // UTF-16
        (Some(buf), _) => {
            if buf.len() % 2 != 0 {
                return Err(Error::Protocol("nvarchar: invalid plp length".into()));
            }

            Ok(Some(decode_utf16le(&buf).into()))
        }
        _ => Ok(None),
    }
}

/// Decodes `buf` in the collation's code page.
pub(crate) fn decode_varchar(buf: Vec<u8>, collation: &Collation) -> crate::Result<String> {
    let encoder = collation.encoding()?;
    let decoded = encoder
        .decode_without_bom_handling_and_without_replacement(&buf)
        .ok_or_else(|| Error::Encoding("invalid sequence".into()))?;

    match decoded {
        // Borrowed means the bytes are valid UTF-8 as they are, so keep them
        // instead of copying.
        Cow::Borrowed(_) => {
            String::from_utf8(buf).map_err(|_| Error::Encoding("invalid sequence".into()))
        }
        Cow::Owned(s) => Ok(s),
    }
}

/// Encodes `s` in the collation's code page; a character it lacks is an error.
pub(crate) fn encode_varchar(s: &str, collation: &Collation) -> crate::Result<Vec<u8>> {
    let mut encoder = collation.encoding()?.new_encoder();
    let len = encoder
        .max_buffer_length_from_utf8_without_replacement(s.len())
        .ok_or_else(|| Error::Encoding("string too long to encode".into()))?;
    let mut bytes = Vec::with_capacity(len);
    let (res, _) = encoder.encode_from_utf8_to_vec_without_replacement(s, &mut bytes, true);
    match res {
        encoding_rs::EncoderResult::Unmappable(_) => {
            Err(Error::Encoding("unrepresentable character".into()))
        }
        _ => Ok(bytes),
    }
}

/// Decodes UTF-16LE bytes (even length) into a `String` allocated at its exact
/// UTF-8 size. ASCII-only input is copied byte by byte without decoding.
///
/// An unpaired surrogate becomes U+FFFD. SQL Server stores them (it treats
/// nvarchar as UCS-2) and SqlClient returns them as-is in a UTF-16 .NET
/// string; a Rust `String` cannot hold one, and failing would lose the whole
/// result set.
pub(crate) fn decode_utf16le(buf: &[u8]) -> String {
    let (pairs, _) = buf.as_chunks::<2>();
    let units = pairs.iter().map(|&pair| u16::from_le_bytes(pair));

    // A surrogate half counts 2, so a valid pair counts 4; an unpaired half
    // becomes 3 bytes and may make the String grow once.
    let mut utf8_len = 0usize;
    let mut ascii = true;
    for u in units.clone() {
        utf8_len += match u {
            0..=0x7f => 1,
            0x80..=0x7ff | 0xd800..=0xdfff => 2,
            _ => 3,
        };
        ascii &= u < 0x80;
    }

    if ascii {
        let bytes: Vec<u8> = pairs.iter().map(|&[lo, _]| lo).collect();
        return String::from_utf8(bytes).expect("ASCII is UTF-8");
    }

    let mut s = String::with_capacity(utf8_len);
    s.extend(char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn decode_utf16le_ascii_and_multibyte() {
        assert_eq!(decode_utf16le(&utf16le("")), "");
        assert_eq!(decode_utf16le(&utf16le("hello")), "hello");
        let s = "åäö €𝄞 ascii";
        let decoded = decode_utf16le(&utf16le(s));
        assert_eq!(decoded, s);
        assert_eq!(decoded.capacity(), s.len());
    }

    #[test]
    fn decode_utf16le_replaces_unpaired_surrogates() {
        // Lone high, lone low, reversed pair, then a valid pair.
        let units = [0xd800, b'a' as u16, 0xdc00, 0xde00, 0xd83d, 0xd83d, 0xde00];
        let buf: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(decode_utf16le(&buf), "\u{FFFD}a\u{FFFD}\u{FFFD}\u{FFFD}😀");
    }

    #[tokio::test]
    async fn decode_nvarchar_with_a_lone_surrogate() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
        use bytes::{BufMut, BytesMut};

        let mut buf = BytesMut::new();
        buf.put_u16_le(4);
        buf.put_u16_le(b'x' as u16);
        buf.put_u16_le(0xdbff);
        let s = decode(
            &mut buf.into_sql_read_bytes(),
            VarLenType::NVarchar,
            8000,
            None,
        )
        .await
        .unwrap();
        assert_eq!(s.as_deref(), Some("x\u{FFFD}"));
    }

    #[test]
    fn decode_varchar_keeps_ascii_and_converts_the_rest() {
        let collation = Collation::new(0x0409, 0); // Windows-1252
        assert_eq!(
            decode_varchar(b"plain".to_vec(), &collation).unwrap(),
            "plain"
        );
        assert_eq!(
            decode_varchar(vec![0xe5, 0xe4, 0xf6], &collation).unwrap(),
            "åäö"
        );
    }
}
