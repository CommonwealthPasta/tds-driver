use super::guid::reorder_bytes;
use super::{Decode, Encode};
use crate::{tds, Error, Result};
use byteorder::{BigEndian, LittleEndian, ReadBytesExt, WriteBytesExt};
use bytes::{BufMut, BytesMut};
use std::convert::TryFrom;
use std::io::{Cursor, Read, Write};
use tds::EncryptionLevel;
use uuid::Uuid;

/// The `TRACEID` option (MS-TDS 2.2.6.5, TDS 7.4): the client's connection
/// id, then an activity id and its sequence number, for correlating the
/// connection in server-side traces.
#[allow(unused)]
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct ActivityId {
    connection_id: Uuid,
    id: Uuid,
    sequence: u32,
}

impl ActivityId {
    /// GUID_CONNID, GUID_ActivityID and ActivitySequence.
    const LEN: u16 = 16 + 16 + 4;

    #[allow(dead_code)]
    pub fn new(connection_id: Uuid, id: Uuid, sequence: u32) -> Self {
        Self {
            connection_id,
            id,
            sequence,
        }
    }
}

/// ENCRYPT_CLIENT_CERT (MS-TDS 2.2.6.5): certificate authentication,
/// combined with the ENCRYPTION option's other values.
const ENCRYPT_CLIENT_CERT: u8 = 0x80;

/// The prelogin packet used to initialize a connection
#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq))]
pub struct PreloginMessage {
    /// [BE] token=0x00
    /// Either the driver version or the version of the SQL server
    pub version: u32,
    pub sub_build: u16,
    /// token=0x01
    pub encryption: EncryptionLevel,
    /// The server's ENCRYPTION has ENCRYPT_CLIENT_CERT set; never sent.
    pub client_cert: bool,
    /// token=0x02
    pub instance_name: Option<String>,
    /// [client] threadid for debugging purposes, token=0x03
    pub thread_id: u32,
    /// token=0x04
    pub mars: bool,
    /// token=0x05
    pub activity_id: Option<ActivityId>,
    /// token=0x06
    pub fed_auth_required: bool,
    pub nonce: Option<[u8; 32]>,
}

impl PreloginMessage {
    pub fn new() -> PreloginMessage {
        let driver_version = crate::get_driver_version();
        PreloginMessage {
            version: driver_version as u32,
            sub_build: (driver_version >> 32) as u16,
            encryption: EncryptionLevel::NotSupported,
            client_cert: false,
            instance_name: None,
            thread_id: 0,
            mars: false,
            activity_id: None,
            fed_auth_required: false,
            nonce: None,
        }
    }

    /// Validates the server's answer to the `INSTOPT` prelogin option
    /// (MS-TDS §2.2.6.5).
    ///
    /// When the client sends an instance name, the server replies with a single
    /// `0x00` byte if the instance the connection landed on is the one that was
    /// requested. Any other payload means the server considers the instance
    /// invalid, in which case this returns a protocol error.
    pub fn validate_instance(&self, requested: Option<&str>) -> Result<()> {
        // Nothing to validate if the client never asked for a named instance.
        if requested.is_none() {
            return Ok(());
        }

        match self.instance_name.as_deref() {
            // `0x00` terminator only -> decoded as `None`: instance is valid.
            None => Ok(()),
            Some(other) => Err(Error::Protocol(
                format!(
                    "server rejected the requested instance {:?} (INSTOPT validity byte was non-zero, got {:?})",
                    requested.unwrap_or_default(),
                    other,
                )
                .into(),
            )),
        }
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    /// What to encrypt, given the client's level and the server's answer, per
    /// the tables of MS-TDS 2.2.6.5: the client's `Required` behaves as its
    /// `On`, and `NotSupported` against a server that wants encryption
    /// terminates the connection.
    pub fn negotiated_encryption(&self, expected: EncryptionLevel) -> Result<EncryptionLevel> {
        use EncryptionLevel::*;

        let level = match (expected, self.encryption) {
            // TLS was set up before the prelogin; the value is ignored.
            (Strict, _) => Strict,
            // A certificate is exchanged in the TLS handshake, so certificate
            // authentication without encryption ends the connection.
            (_, NotSupported) | (NotSupported, _) if self.client_cert => {
                return Err(Error::Protocol(
                    "prelogin: the server asks for certificate authentication without encryption"
                        .into(),
                ))
            }
            (_, Strict) => {
                return Err(Error::Protocol(
                    "prelogin: invalid server encryption value 4".into(),
                ))
            }
            (Off, Off) => Off,
            (Off | On | Required, On | Required) => On,
            (NotSupported, NotSupported | Off) => NotSupported,
            // MS-TDS 2.2.6.5 sends the login unencrypted here. Its TLS would
            // have validated the certificate, so a man in the middle could
            // answer NOT_SUP to read it; plaintext is opt-in (`NotSupported`).
            (Off, NotSupported) => {
                return Err(Error::Protocol(
                    "the server does not support encryption, so the login would be sent unencrypted; set Encrypt=DANGER_PLAINTEXT to allow that"
                        .into(),
                ))
            }
            (On | Required, Off | NotSupported) => {
                return Err(Error::Protocol(
                    "Server does not allow the requested encryption level.".into(),
                ))
            }
            (NotSupported, On | Required) => {
                return Err(Error::Protocol(
                    "Server requires encryption, but it is set to NotSupported.".into(),
                ))
            }
        };

        Ok(level)
    }

    #[cfg(not(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    )))]
    pub fn negotiated_encryption(&self, _: EncryptionLevel) -> Result<EncryptionLevel> {
        if self.client_cert {
            return Err(Error::Protocol(
                "prelogin: the server asks for certificate authentication without encryption"
                    .into(),
            ));
        }
        Ok(EncryptionLevel::NotSupported)
    }
}

// prelogin fields
// http://msdn.microsoft.com/en-us/library/dd357559.aspx
const PRELOGIN_VERSION: u8 = 0;
const PRELOGIN_ENCRYPTION: u8 = 1;
const PRELOGIN_INSTOPT: u8 = 2;
const PRELOGIN_THREADID: u8 = 3;
const PRELOGIN_MARS: u8 = 4;
const PRELOGIN_TRACEID: u8 = 5;
const PRELOGIN_FEDAUTHREQUIRED: u8 = 6;
const PRELOGIN_NONCEOPT: u8 = 7;
const PRELOGIN_TERMINATOR: u8 = 0xff;

impl Encode<BytesMut> for PreloginMessage {
    fn encode(self, dst: &mut BytesMut) -> Result<()> {
        let mut fields = Vec::new();
        let mut data_cursor = Cursor::new(Vec::with_capacity(512));

        // version
        fields.push((PRELOGIN_VERSION, 0x04 + 0x02)); // version + subbuild
        data_cursor.write_u32::<BigEndian>(self.version)?;
        data_cursor.write_u16::<BigEndian>(self.sub_build)?;

        // encryption
        fields.push((PRELOGIN_ENCRYPTION, 0x01)); // encryption
        data_cursor.write_u8(self.encryption.as_wire_value())?;

        // instance name (INSTOPT): a null-terminated MBCS string naming the
        // instance the client wants the server to validate. An empty name is
        // encoded as a lone `0x00` terminator.
        let instance = self.instance_name.as_deref().unwrap_or_default();
        let instance_bytes = instance.as_bytes();
        let too_long = || {
            Error::Conversion(
                format!(
                    "PRELOGIN: an instance name of {} bytes is too long for its u16 offsets",
                    instance_bytes.len()
                )
                .into(),
            )
        };
        let instopt_len = u16::try_from(instance_bytes.len() + 1).map_err(|_| too_long())?;
        fields.push((PRELOGIN_INSTOPT, instopt_len));
        data_cursor.write_all(instance_bytes)?;
        data_cursor.write_u8(0x00)?; // null terminator

        // threadid
        fields.push((PRELOGIN_THREADID, 0x04)); // thread id
        data_cursor.write_u32::<BigEndian>(self.thread_id)?;

        // MARS
        fields.push((PRELOGIN_MARS, 0x01)); // MARS
        data_cursor.write_u8(self.mars as u8)?;

        if let Some(trace) = self.activity_id.as_ref() {
            fields.push((PRELOGIN_TRACEID, ActivityId::LEN));
            for id in [trace.connection_id, trace.id] {
                let mut data = *id.as_bytes();
                reorder_bytes(&mut data);
                data_cursor.write_all(&data)?;
            }
            data_cursor.write_u32::<LittleEndian>(trace.sequence)?;
        }

        // fed auth
        if self.fed_auth_required {
            fields.push((PRELOGIN_FEDAUTHREQUIRED, 0x01));
            data_cursor.write_u8(0x01)?;
        }

        // build the packet-body
        // offset = PL_OPTION_TOKEN + PL_OFFSET + PL_OPTION_LENGTH = 5 bytes + the terminator (0xFF)
        let mut data_offset = (fields.len() * 5 + 1) as u16;

        // write the offset table
        for (token, length) in fields {
            dst.put_u8(token);
            dst.put_u16(data_offset);
            dst.put_u16(length);

            data_offset = data_offset.checked_add(length).ok_or_else(too_long)?;
        }

        dst.put_u8(PRELOGIN_TERMINATOR);
        dst.extend(data_cursor.into_inner());

        Ok(())
    }
}

impl Decode<BytesMut> for PreloginMessage {
    fn decode(src: &mut BytesMut) -> Result<Self>
    where
        Self: Sized,
    {
        let src: &[u8] = src;
        let mut table = Cursor::new(src);
        let mut ret = PreloginMessage::new();

        loop {
            let token = table.read_u8()?;

            if token == PRELOGIN_TERMINATOR {
                break;
            }

            let offset = table.read_u16::<BigEndian>()? as usize;
            let length = table.read_u16::<BigEndian>()?;

            // Each option is read only from its own bytes.
            let data = src
                .get(offset..offset + length as usize)
                .ok_or_else(|| Error::Protocol("prelogin: option data out of bounds".into()))?;
            let mut cursor = Cursor::new(data);

            match token {
                // version
                PRELOGIN_VERSION => {
                    ret.version = cursor.read_u32::<BigEndian>()?;
                    ret.sub_build = cursor.read_u16::<BigEndian>()?;
                }
                // encryption
                PRELOGIN_ENCRYPTION => {
                    let encrypt = cursor.read_u8()?;
                    ret.client_cert = encrypt & ENCRYPT_CLIENT_CERT != 0;
                    let level = encrypt & !ENCRYPT_CLIENT_CERT;
                    ret.encryption = tds::EncryptionLevel::try_from(level).map_err(|_| {
                        Error::Protocol(format!("invalid encryption value: {}", encrypt).into())
                    })?;
                }
                // The server answers with one validity byte, 0 or 1; the
                // terminating NUL is optional.
                PRELOGIN_INSTOPT => {
                    let bytes = data.split(|b| *b == 0).next().unwrap_or_default();

                    if !bytes.is_empty() {
                        ret.instance_name = Some(String::from_utf8_lossy(bytes).into_owned());
                    }
                }
                // The server SHOULD send it empty; it is for debugging only.
                PRELOGIN_THREADID => {
                    ret.thread_id = if length == 0 {
                        0
                    } else if length == 4 {
                        cursor.read_u32::<BigEndian>()?
                    } else {
                        return Err(Error::Protocol(
                            format!("prelogin: invalid threadid length: {}", length).into(),
                        ));
                    }
                }
                PRELOGIN_MARS => {
                    ret.mars = data.first().is_some_and(|b| *b != 0);
                }
                // Other lengths are ignored, as SqlClient ignores the option.
                PRELOGIN_TRACEID if length == ActivityId::LEN => {
                    let mut guid = || {
                        let mut data = [0u8; 16];
                        cursor.read_exact(&mut data)?;
                        reorder_bytes(&mut data);
                        Result::Ok(Uuid::from_bytes(data))
                    };
                    let (connection_id, id) = (guid()?, guid()?);

                    ret.activity_id = Some(ActivityId {
                        connection_id,
                        id,
                        sequence: cursor.read_u32::<LittleEndian>()?,
                    });
                }
                PRELOGIN_FEDAUTHREQUIRED => {
                    ret.fed_auth_required = data.first().is_some_and(|b| *b != 0);
                }
                PRELOGIN_NONCEOPT => {
                    let mut nonce = [0u8; 32];
                    cursor.read_exact(&mut nonce)?;
                    ret.nonce = Some(nonce);
                }
                // Options of newer TDS versions; offset and length let us skip them.
                _ => {}
            }
        }

        Ok(ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses the PRELOGIN option-offset table and returns the option tokens in
    /// the order they were emitted, stopping at the terminator.
    fn option_tokens(bytes: &[u8]) -> Vec<u8> {
        let mut tokens = Vec::new();
        let mut pos = 0;

        while pos < bytes.len() {
            let token = bytes[pos];

            if token == PRELOGIN_TERMINATOR {
                break;
            }

            tokens.push(token);
            // token (1) + offset (2) + length (2)
            pos += 5;
        }

        tokens
    }

    /// Reads the raw payload bytes for a given option token from an encoded
    /// PRELOGIN packet using its offset-table entry.
    fn option_payload(bytes: &[u8], token: u8) -> Option<Vec<u8>> {
        let mut pos = 0;

        while pos < bytes.len() && bytes[pos] != PRELOGIN_TERMINATOR {
            if bytes[pos] == token {
                let offset = u16::from_be_bytes([bytes[pos + 1], bytes[pos + 2]]) as usize;
                let length = u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]) as usize;

                return Some(bytes[offset..offset + length].to_vec());
            }

            pos += 5;
        }

        None
    }

    #[test]
    fn prelogin_always_emits_instopt() {
        let mut payload = BytesMut::new();
        PreloginMessage::new()
            .encode(&mut payload)
            .expect("encode should succeed");

        assert!(option_tokens(&payload).contains(&PRELOGIN_INSTOPT));
        // An empty instance name is a single null terminator byte.
        assert_eq!(option_payload(&payload, PRELOGIN_INSTOPT), Some(vec![0x00]));
    }

    #[test]
    fn prelogin_emits_named_instance() {
        let mut payload = BytesMut::new();
        let mut prelogin = PreloginMessage::new();
        prelogin.instance_name = Some("MSSQLServer".to_string());
        prelogin
            .clone()
            .encode(&mut payload)
            .expect("encode should succeed");

        let expected: Vec<u8> = b"MSSQLServer\0".to_vec();
        assert_eq!(option_payload(&payload, PRELOGIN_INSTOPT), Some(expected));

        let decoded = PreloginMessage::decode(&mut payload).expect("decode should succeed");
        assert_eq!(decoded.instance_name.as_deref(), Some("MSSQLServer"));
    }

    #[test]
    fn prelogin_emits_traceid_only_when_present() {
        let mut without = BytesMut::new();
        PreloginMessage::new()
            .encode(&mut without)
            .expect("encode should succeed");
        assert!(!option_tokens(&without).contains(&PRELOGIN_TRACEID));

        let mut with = BytesMut::new();
        let mut prelogin = PreloginMessage::new();
        prelogin.activity_id = Some(ActivityId::new(
            Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap(),
            Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap(),
            0x0102_0304,
        ));
        prelogin
            .clone()
            .encode(&mut with)
            .expect("encode should succeed");

        // GUID_CONNID, GUID_ActivityID (both as .NET writes a Guid), then
        // ActivitySequence, little-endian: 36 bytes.
        let mut expected = vec![
            0xff, 0x19, 0x96, 0x6f, 0x86, 0x8b, 0x11, 0xd0, 0xb4, 0x2d, 0x00, 0xc0, 0x4f, 0xc9,
            0x64, 0xff,
        ];
        expected.extend_from_slice(&[
            0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]);
        expected.extend_from_slice(&[4, 3, 2, 1]);
        assert_eq!(option_payload(&with, PRELOGIN_TRACEID), Some(expected));

        let decoded = PreloginMessage::decode(&mut with).expect("decode should succeed");
        assert_eq!(decoded.activity_id, prelogin.activity_id);

        // A TRACEID of another length is skipped, not misread.
        let mut buf = response(&[(PRELOGIN_TRACEID, &[0; 20]), (PRELOGIN_MARS, &[1])]);
        let decoded = PreloginMessage::decode(&mut buf).unwrap();
        assert_eq!((decoded.activity_id, decoded.mars), (None, true));
    }

    #[test]
    fn decode_reads_the_client_cert_bit() {
        use EncryptionLevel::*;
        for (byte, level, cert) in [
            (0x80, Off, true),
            (0x81, On, true),
            (0x82, NotSupported, true),
            (0x83, Required, true),
            (0x01, On, false),
        ] {
            let mut buf = response(&[(PRELOGIN_VERSION, VERSION), (PRELOGIN_ENCRYPTION, &[byte])]);
            let decoded = PreloginMessage::decode(&mut buf).unwrap();
            assert_eq!(
                (decoded.encryption, decoded.client_cert),
                (level, cert),
                "{byte:#x}"
            );
        }
        for byte in [0x20, 0x40, 0x85] {
            let mut buf = response(&[(PRELOGIN_ENCRYPTION, &[byte])]);
            assert!(PreloginMessage::decode(&mut buf).is_err(), "{byte:#x}");
        }
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn client_cert_needs_encryption() {
        use EncryptionLevel::*;
        let neg = |client, server| {
            let mut prelogin = PreloginMessage::new();
            prelogin.encryption = server;
            prelogin.client_cert = true;
            prelogin.negotiated_encryption(client).ok()
        };
        assert_eq!(neg(Off, Off), Some(Off));
        assert_eq!(neg(Off, Required), Some(On));
        assert_eq!(neg(Required, On), Some(On));
        assert_eq!(neg(Strict, NotSupported), Some(Strict));
        assert_eq!(neg(NotSupported, Off), None);
        assert_eq!(neg(NotSupported, NotSupported), None);
        assert_eq!(neg(Off, NotSupported), None);
    }

    #[test]
    fn decode_accepts_zero_length_threadid() {
        // A THREADID option with length 0 must decode to thread_id 0, not error.
        // Table entry (token, offset=6, length=0) then the terminator.
        let mut buf = BytesMut::from(
            &[
                PRELOGIN_THREADID,
                0x00,
                0x06,
                0x00,
                0x00,
                PRELOGIN_TERMINATOR,
            ][..],
        );
        let decoded = PreloginMessage::decode(&mut buf).expect("zero-length threadid must decode");
        assert_eq!(decoded.thread_id, 0);
    }

    #[test]
    fn decode_reads_nonce_option() {
        // Table entry for NONCEOPT (offset 6, length 32) + terminator + 32 bytes.
        let mut bytes = vec![
            PRELOGIN_NONCEOPT,
            0x00,
            0x06,
            0x00,
            0x20,
            PRELOGIN_TERMINATOR,
        ];
        bytes.extend_from_slice(&[0xAB; 32]);
        let mut buf = BytesMut::from(&bytes[..]);

        let decoded = PreloginMessage::decode(&mut buf).expect("nonce option must decode");
        assert_eq!(decoded.nonce, Some([0xAB; 32]));
    }

    #[test]
    fn option_payload_returns_none_for_absent_token() {
        let mut payload = BytesMut::new();
        PreloginMessage::new()
            .encode(&mut payload)
            .expect("encode should succeed");

        // A fresh message emits no TRACEID option.
        assert_eq!(option_payload(&payload, PRELOGIN_TRACEID), None);
    }

    #[test]
    fn decode_rejects_invalid_encryption_value() {
        // ENCRYPTION option (offset 6, length 1) + terminator + an out-of-range
        // encryption byte.
        let mut buf = BytesMut::from(
            &[
                PRELOGIN_ENCRYPTION,
                0x00,
                0x06,
                0x00,
                0x01,
                PRELOGIN_TERMINATOR,
                0x63, // 99: not a valid EncryptionLevel
            ][..],
        );

        match PreloginMessage::decode(&mut buf) {
            Err(Error::Protocol(_)) => {}
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_invalid_threadid_length() {
        // THREADID option with an unsupported length (2) must error.
        let mut buf = BytesMut::from(
            &[
                PRELOGIN_THREADID,
                0x00,
                0x06,
                0x00,
                0x02,
                PRELOGIN_TERMINATOR,
                0x00,
                0x00,
            ][..],
        );

        match PreloginMessage::decode(&mut buf) {
            Err(Error::Protocol(_)) => {}
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    /// A server PRELOGIN answer built from `(token, data)` options.
    fn response(opts: &[(u8, &[u8])]) -> BytesMut {
        let mut table = Vec::new();
        let mut data = Vec::new();
        let mut offset = opts.len() * 5 + 1;
        for (token, d) in opts {
            table.push(*token);
            table.extend_from_slice(&(offset as u16).to_be_bytes());
            table.extend_from_slice(&(d.len() as u16).to_be_bytes());
            offset += d.len();
            data.extend_from_slice(d);
        }
        table.push(PRELOGIN_TERMINATOR);
        table.extend(data);
        BytesMut::from(&table[..])
    }

    const VERSION: &[u8] = &[16, 0, 0x07, 0xd0, 0, 0];

    #[test]
    fn decode_skips_unknown_tokens() {
        let mut buf = response(&[
            (PRELOGIN_VERSION, VERSION),
            (0x08, &[0xAA, 0xBB]),
            (PRELOGIN_ENCRYPTION, &[EncryptionLevel::On as u8]),
            (0x50, &[]),
            (PRELOGIN_MARS, &[1]),
        ]);
        let decoded = PreloginMessage::decode(&mut buf).expect("unknown tokens are skipped");
        assert_eq!(decoded.encryption, EncryptionLevel::On);
        assert!(decoded.mars);
    }

    #[test]
    fn decode_accepts_nonempty_threadid() {
        let mut buf = response(&[
            (PRELOGIN_VERSION, VERSION),
            (PRELOGIN_THREADID, &[0, 0, 0, 7]),
        ]);
        let decoded = PreloginMessage::decode(&mut buf).unwrap();
        assert_eq!(decoded.thread_id, 7);
    }

    #[test]
    fn decode_reads_instopt_without_terminator() {
        // One validity byte as the last option byte of the message.
        let mut buf = response(&[(PRELOGIN_VERSION, VERSION), (PRELOGIN_INSTOPT, &[1])]);
        let decoded = PreloginMessage::decode(&mut buf).expect("1-byte INSTOPT must decode");
        assert!(decoded.validate_instance(Some("INST")).is_err());

        let mut buf = response(&[(PRELOGIN_INSTOPT, &[0]), (PRELOGIN_VERSION, VERSION)]);
        let decoded = PreloginMessage::decode(&mut buf).unwrap();
        assert!(decoded.validate_instance(Some("INST")).is_ok());
    }

    #[test]
    fn decode_reads_options_only_from_their_own_bytes() {
        // An empty MARS option must not read the next option's data.
        let mut buf = response(&[(PRELOGIN_MARS, &[]), (PRELOGIN_FEDAUTHREQUIRED, &[1])]);
        let decoded = PreloginMessage::decode(&mut buf).unwrap();
        assert!(!decoded.mars);
        assert!(decoded.fed_auth_required);

        // Data past the end of the message is an error, not a panic.
        let mut buf = BytesMut::from(&[PRELOGIN_MARS, 0x00, 0x06, 0x00, 0x09, 0xff][..]);
        assert!(matches!(
            PreloginMessage::decode(&mut buf),
            Err(Error::Protocol(_))
        ));
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn negotiated_encryption_follows_the_ms_tds_table() {
        use EncryptionLevel::*;
        let neg = |client, server| {
            let mut prelogin = PreloginMessage::new();
            prelogin.encryption = server;
            prelogin.negotiated_encryption(client).ok()
        };

        for (client, server, expected) in [
            (Off, Off, Some(Off)),
            (Off, On, Some(On)),
            (Off, Required, Some(On)),
            (Off, NotSupported, None),
            (On, Off, None),
            (On, On, Some(On)),
            (On, Required, Some(On)),
            (On, NotSupported, None),
            (Required, Off, None),
            (Required, On, Some(On)),
            (Required, Required, Some(On)),
            (Required, NotSupported, None),
            (NotSupported, NotSupported, Some(NotSupported)),
            (NotSupported, Off, Some(NotSupported)),
            (NotSupported, On, None),
            (NotSupported, Required, None),
            (Off, Strict, None),
            (Strict, NotSupported, Some(Strict)),
        ] {
            assert_eq!(
                neg(client, server),
                expected,
                "client {client:?}, server {server:?}"
            );
        }
    }

    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn negotiated_encryption_off_and_strict() {
        let mut prelogin = PreloginMessage::new();

        // Both sides Off -> Off.
        prelogin.encryption = EncryptionLevel::Off;
        assert_eq!(
            prelogin
                .negotiated_encryption(EncryptionLevel::Off)
                .unwrap(),
            EncryptionLevel::Off
        );

        // Strict is negotiated out-of-band; it stays Strict regardless of the
        // server's advertised level.
        assert_eq!(
            prelogin
                .negotiated_encryption(EncryptionLevel::Strict)
                .unwrap(),
            EncryptionLevel::Strict
        );
    }

    #[test]
    fn validate_instance_accepts_valid_response() {
        // Server valid response = lone 0x00 -> decoded as `None`.
        let msg = PreloginMessage::new();
        assert!(msg.validate_instance(Some("MSSQLServer")).is_ok());
        // No requested instance -> nothing to validate.
        assert!(msg.validate_instance(None).is_ok());
    }

    #[test]
    fn validate_instance_rejects_invalid_response() {
        let mut msg = PreloginMessage::new();
        msg.instance_name = Some("otherinstance".to_string());
        assert!(msg.validate_instance(Some("MSSQLServer")).is_err());
    }

    #[test]
    fn prelogin_roundtrip() {
        let mut payload = BytesMut::new();
        let prelogin = PreloginMessage::new();
        prelogin
            .clone()
            .encode(&mut payload)
            .expect("encode should succeed");

        let decoded = PreloginMessage::decode(&mut payload).expect("decode should succeed");

        assert_eq!(prelogin, decoded);
    }

    #[test]
    fn prelogin_with_fedauth_roundtrip() {
        let mut payload = BytesMut::new();
        let mut prelogin = PreloginMessage::new();
        prelogin.fed_auth_required = true;
        prelogin
            .clone()
            .encode(&mut payload)
            .expect("encode should succeed");

        let decoded = PreloginMessage::decode(&mut payload).expect("decode should succeed");

        assert_eq!(prelogin, decoded);
    }

    // #425: a server declining the requested encryption level must yield a
    // catchable protocol error instead of panicking.
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    #[test]
    fn negotiated_encryption_rejects_declined_level() {
        let mut prelogin = PreloginMessage::new();
        // Server responds with an encryption level the client did not offer /
        // that is weaker than the required `On`.
        prelogin.encryption = EncryptionLevel::Off;

        let result = prelogin.negotiated_encryption(EncryptionLevel::On);

        match result {
            Err(Error::Protocol(_)) => {}
            other => panic!("expected Err(Error::Protocol), got {:?}", other),
        }

        // A matching, valid negotiation still succeeds.
        prelogin.encryption = EncryptionLevel::On;
        assert_eq!(
            prelogin.negotiated_encryption(EncryptionLevel::On).unwrap(),
            EncryptionLevel::On
        );
    }

    #[test]
    fn instance_name_past_the_u16_offsets_is_an_error() {
        // 65,534 bytes overflowed the offsets (a debug-build panic), 70,000
        // wrapped INSTOPT's length.
        for len in [65_534, 70_000] {
            let mut msg = PreloginMessage::new();
            msg.instance_name = Some("a".repeat(len));
            let err = msg.encode(&mut BytesMut::new()).unwrap_err();
            assert!(matches!(err, Error::Conversion(_)), "{len}: {err:?}");
        }
    }
}
