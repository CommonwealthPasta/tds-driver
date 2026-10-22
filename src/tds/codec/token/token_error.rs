use crate::{tds::codec::TokenInfo, SqlReadBytes};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
/// An error token returned from the server.
pub struct TokenError {
    /// ErrorCode
    pub(crate) code: u32,
    /// ErrorState (describing code)
    pub(crate) state: u8,
    /// The class (severity) of the error
    pub(crate) class: u8,
    /// The error message
    pub(crate) message: String,
    // `Box<str>` keeps `Error`, and so every `crate::Result` on the read
    // path, small.
    pub(crate) server: Box<str>,
    pub(crate) procedure: Box<str>,
    pub(crate) line: u32,
    /// The response's other messages, attached only to the error a request
    /// fails with. Boxed so `Error` stays small.
    pub(crate) related: Option<Box<RelatedMessages>>,
}

/// The ERROR and INFO tokens of a response besides its first ERROR. SqlClient
/// reports all of them too: in `SqlException.Errors`, warnings after errors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RelatedMessages {
    pub(crate) errors: Vec<TokenError>,
    pub(crate) infos: Vec<TokenError>,
}

impl RelatedMessages {
    pub(crate) fn is_empty(&self) -> bool {
        self.errors.is_empty() && self.infos.is_empty()
    }
}

impl From<&TokenInfo> for TokenError {
    fn from(info: &TokenInfo) -> Self {
        Self {
            code: info.number,
            state: info.state,
            class: info.class,
            message: info.message.clone(),
            server: info.server.as_str().into(),
            procedure: info.procedure.as_str().into(),
            line: info.line,
            related: None,
        }
    }
}

impl TokenError {
    pub(crate) async fn decode<R>(src: &mut R) -> crate::Result<Self>
    where
        R: SqlReadBytes + Unpin,
    {
        let _length = src.read_u16_le().await? as usize;

        let code = src.read_u32_le().await?;
        let state = src.read_u8().await?;
        let class = src.read_u8().await?;

        let message = src.read_us_varchar().await?;
        let server = src.read_b_varchar().await?;
        let procedure = src.read_b_varchar().await?;

        // MS-TDS 2.2.7.10: a 4-byte LONG since TDS 7.2, which LOGINACK
        // requires. SqlClient always reads 4 bytes, also before LOGINACK.
        let line = src.read_u32_le().await?;

        let token = TokenError {
            code,
            state,
            class,
            message,
            server: server.into(),
            procedure: procedure.into(),
            line,
            related: None,
        };

        Ok(token)
    }

    /// The error code, see descriptions from [the manual].
    ///
    /// [the manual]: https://docs.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors?view=sql-server-ver15
    pub fn code(&self) -> u32 {
        self.code
    }

    /// The error state, used as a modifier to the error number.
    pub fn state(&self) -> u8 {
        self.state
    }

    /// The class (severity) of the error. A class of less than 10 indicates an
    /// informational message.
    pub fn class(&self) -> u8 {
        self.class
    }

    /// The error message returned from the server.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The server name.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The name of the stored procedure causing the error.
    pub fn procedure(&self) -> &str {
        &self.procedure
    }

    /// The line number in the SQL batch or stored procedure that caused the
    /// error. Line numbers begin at 1. If the line number is not applicable to
    /// the message, the value is 0.
    pub fn line(&self) -> u32 {
        self.line
    }

    /// The ERROR tokens that followed this one in the same response, in
    /// order. A batch keeps running after most errors, so later statements
    /// can fail too. Empty on the entries themselves.
    pub fn other_errors(&self) -> &[TokenError] {
        self.related.as_deref().map_or(&[], |r| &r.errors)
    }

    /// The INFO messages of the same response, before and after this error,
    /// in order: `PRINT` output, `RAISERROR` at severity 10 or less, and
    /// server notices. [`code`](Self::code) is the message number.
    pub fn info_messages(&self) -> &[TokenError] {
        self.related.as_deref().map_or(&[], |r| &r.infos)
    }
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' on server {} executing {} on line {} (code: {}, state: {}, class: {})",
            self.message, self.server, self.procedure, self.line, self.code, self.state, self.class
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TokenError {
        TokenError {
            code: 1205,
            state: 2,
            class: 13,
            message: "deadlocked".to_string(),
            server: "myserver".into(),
            procedure: "myproc".into(),
            line: 42,
            related: None,
        }
    }

    #[test]
    fn accessors() {
        let e = sample();
        assert_eq!(e.code(), 1205);
        assert_eq!(e.state(), 2);
        assert_eq!(e.class(), 13);
        assert_eq!(e.message(), "deadlocked");
        assert_eq!(e.server(), "myserver");
        assert_eq!(e.procedure(), "myproc");
        assert_eq!(e.line(), 42);
        assert!(e.other_errors().is_empty());
        assert!(e.info_messages().is_empty());
    }

    #[test]
    fn display_contains_all_fields() {
        let rendered = format!("{}", sample());
        assert_eq!(
            rendered,
            "'deadlocked' on server myserver executing myproc on line 42 (code: 1205, state: 2, class: 13)"
        );
    }

    #[tokio::test]
    async fn decode_reads_all_fields_with_four_byte_line_number() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
        use byteorder::{LittleEndian, WriteBytesExt};
        use bytes::{BufMut, BytesMut};

        fn write_us_varchar(buf: &mut Vec<u8>, s: &str) {
            buf.write_u16::<LittleEndian>(s.encode_utf16().count() as u16)
                .unwrap();
            for u in s.encode_utf16() {
                buf.write_u16::<LittleEndian>(u).unwrap();
            }
        }

        fn write_b_varchar(buf: &mut Vec<u8>, s: &str) {
            buf.push(s.encode_utf16().count() as u8);
            for u in s.encode_utf16() {
                buf.write_u16::<LittleEndian>(u).unwrap();
            }
        }

        let mut body = Vec::new();
        body.write_u32::<LittleEndian>(1205).unwrap(); // code
        body.push(2); // state
        body.push(13); // class
        write_us_varchar(&mut body, "deadlocked");
        write_b_varchar(&mut body, "myserver");
        write_b_varchar(&mut body, "myproc");
        body.write_u32::<LittleEndian>(42).unwrap(); // line

        let mut buf = BytesMut::new();
        buf.put_u16_le(body.len() as u16); // length prefix, ignored by decode
        buf.put_slice(&body);

        let decoded = TokenError::decode(&mut buf.into_sql_read_bytes())
            .await
            .unwrap();

        assert_eq!(decoded, sample());
    }

    #[tokio::test]
    async fn decode_reads_full_four_byte_line_number_on_tds72_plus() {
        // 0x0001_0001 reads as 1 from 2 bytes, as 65537 from all 4.
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
        use bytes::{BufMut, BytesMut};

        let mut body = BytesMut::new();
        body.put_u32_le(1205); // code
        body.put_u8(2); // state
        body.put_u8(13); // class
        body.put_u16_le(0); // message: us_varchar, length 0
        body.put_u8(0); // server: b_varchar, length 0
        body.put_u8(0); // procedure: b_varchar, length 0
        body.put_u32_le(0x0001_0001); // line number, 4 bytes

        let mut buf = BytesMut::new();
        buf.put_u16_le(body.len() as u16); // length prefix, ignored by decode
        buf.put_slice(&body);

        let decoded = TokenError::decode(&mut buf.into_sql_read_bytes())
            .await
            .unwrap();

        assert_eq!(decoded.line(), 0x0001_0001);
    }
}
