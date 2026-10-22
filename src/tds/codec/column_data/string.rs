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

            Ok(Some(decode_utf16le(&buf)?.into()))
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

/// Decodes UTF-16LE bytes (even length) into a `String` allocated at its exact
/// UTF-8 size. ASCII-only input is copied byte by byte without decoding.
pub(crate) fn decode_utf16le(buf: &[u8]) -> crate::Result<String> {
    let pairs = buf.as_chunks::<2>().0;
    let units = pairs.iter().map(|&pair| u16::from_le_bytes(pair));

    // A surrogate half counts 2, so a valid pair counts 4. Unpaired halves
    // fail below anyway.
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
        let bytes: Vec<u8> = pairs.iter().map(|pair| pair[0]).collect();
        return String::from_utf8(bytes)
            .map_err(|_| Error::Protocol("nvarchar: invalid UTF-16 sequence".into()));
    }

    let mut s = String::with_capacity(utf8_len);
    for c in char::decode_utf16(units) {
        s.push(c.map_err(|_| Error::Protocol("nvarchar: invalid UTF-16 sequence".into()))?);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn decode_utf16le_ascii_and_multibyte() {
        assert_eq!(decode_utf16le(&utf16le("")).unwrap(), "");
        assert_eq!(decode_utf16le(&utf16le("hello")).unwrap(), "hello");
        let s = "åäö €𝄞 ascii";
        let decoded = decode_utf16le(&utf16le(s)).unwrap();
        assert_eq!(decoded, s);
        assert_eq!(decoded.capacity(), s.len());
    }

    #[test]
    fn decode_utf16le_rejects_an_unpaired_surrogate() {
        let err = decode_utf16le(&[0x00, 0xd8, b'a', 0]).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
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
