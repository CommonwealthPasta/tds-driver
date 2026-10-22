//! PRELOGIN negotiation against an in-process fake server.

use std::time::Duration;
use tds_driver::{error::Error, AuthMethod, Client, Config, EncryptionLevel};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_util::compat::TokioAsyncWriteCompatExt;

const OFF: u8 = 0;
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
const ON: u8 = 1;
const NOT_SUP: u8 = 2;
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
const REQ: u8 = 3;

fn packet(ty: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 8) as u16;
    let mut p = vec![ty, 1, (len >> 8) as u8, len as u8, 0, 0, 1, 0];
    p.extend_from_slice(payload);
    p
}

/// A PRELOGIN answer built from `(token, data)` options.
fn prelogin(opts: &[(u8, &[u8])]) -> Vec<u8> {
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
    table.push(0xff);
    table.extend(data);
    table
}

const VERSION: (u8, &[u8]) = (0x00, &[16, 0, 0x07, 0xd0, 0, 0]);

/// LOGINACK (TDS 7.4) followed by DONE.
fn login_response() -> Vec<u8> {
    let name: Vec<u8> = "Fake".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut body = vec![1u8];
    body.extend_from_slice(&0x7400_0004u32.to_be_bytes());
    body.push(4);
    body.extend_from_slice(&name);
    body.extend_from_slice(&[16, 0, 0x07, 0xd0]);
    let mut t = vec![0xAD];
    t.extend_from_slice(&(body.len() as u16).to_le_bytes());
    t.extend(body);
    t.extend_from_slice(&[0xFD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    t
}

/// Reads one message; returns the type of its first packet.
async fn read_message(s: &mut TcpStream) -> std::io::Result<u8> {
    Ok(read_message_body(s).await?.0)
}

/// Reads one message; returns the type of its first packet and its payload.
async fn read_message_body(s: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut ty = None;
    let mut payload = Vec::new();
    loop {
        let mut h = [0u8; 8];
        s.read_exact(&mut h).await?;
        let mut body = vec![0u8; u16::from_be_bytes([h[2], h[3]]) as usize - 8];
        s.read_exact(&mut body).await?;
        payload.extend(body);
        let ty = *ty.get_or_insert(h[0]);
        if h[1] & 1 == 1 {
            return Ok((ty, payload));
        }
    }
}

/// Answers the client's PRELOGIN with `answer` and a LOGIN7 with a login
/// acknowledgement. Reports the type of the packet that followed PRELOGIN,
/// or `None` if the client closed the connection.
async fn fake_server(answer: Vec<u8>) -> (u16, oneshot::Receiver<Option<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        read_message(&mut s).await.unwrap();
        s.write_all(&packet(4, &answer)).await.unwrap();
        let next = read_message(&mut s).await.ok();
        if next == Some(0x10) {
            s.write_all(&packet(4, &login_response())).await.unwrap();
        }
        let _ = tx.send(next);
        // Keep a login open until the client has read the reply; close on
        // anything else, such as a TLS handshake.
        if next == Some(0x10) {
            let _ = s.read(&mut [0u8; 1]).await;
        }
    });

    (port, rx)
}

async fn connect(
    answer: Vec<u8>,
    encryption: EncryptionLevel,
    instance: Option<&str>,
) -> (tds_driver::Result<()>, Option<u8>) {
    let (port, seen) = fake_server(answer).await;
    let mut config = Config::new();
    config.host("127.0.0.1");
    if let Some(instance) = instance {
        config.instance_name(instance);
    } else {
        config.port(port);
    }
    config.encryption(encryption);
    config.authentication(AuthMethod::sql_server("sa", "x"));

    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let res = tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect(config, tcp.compat_write()),
    )
    .await
    .expect("connect timed out")
    .map(drop);
    (res, seen.await.unwrap_or(None))
}

fn encryption(value: u8) -> Vec<u8> {
    prelogin(&[VERSION, (0x01, &[value])])
}

/// MS-TDS 2.2.6.5: no encryption when either side lacks it, if the client
/// allows that.
#[tokio::test]
async fn login_without_tls_when_one_side_does_not_support_it() {
    for (client, server) in [
        (EncryptionLevel::NotSupported, OFF),
        (EncryptionLevel::NotSupported, NOT_SUP),
    ] {
        let (res, seen) = connect(encryption(server), client, None).await;
        assert!(res.is_ok(), "{client:?}/{server}: {res:?}");
        assert_eq!(seen, Some(0x10), "{client:?}/{server}: LOGIN7 in the clear");
    }
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
#[tokio::test]
async fn mismatched_encryption_fails_before_tls() {
    for (client, server) in [
        (EncryptionLevel::Required, NOT_SUP),
        (EncryptionLevel::Required, OFF),
        (EncryptionLevel::NotSupported, REQ),
        // A login that would have had TLS isn't sent without it.
        (EncryptionLevel::Off, NOT_SUP),
    ] {
        let (res, seen) = connect(encryption(server), client, None).await;
        assert!(
            matches!(res, Err(Error::Protocol(_))),
            "{client:?}/{server}: {res:?}"
        );
        assert_eq!(
            seen, None,
            "{client:?}/{server}: nothing sent after PRELOGIN"
        );
    }
}

/// Unknown options and a THREADID are tolerated.
#[tokio::test]
async fn prelogin_with_unknown_options_and_thread_id() {
    let answer = prelogin(&[
        VERSION,
        (0x01, &[NOT_SUP]),
        (0x03, &[0, 0, 0, 7]),
        (0x08, &[1, 2, 3]),
        (0x02, &[0]),
    ]);
    let (res, _) = connect(answer, EncryptionLevel::NotSupported, None).await;
    assert!(res.is_ok(), "{res:?}");
}

/// A rejected instance is reported as such, also when the validity byte is
/// the last byte of the message.
#[tokio::test]
async fn instance_rejected_by_one_byte_instopt() {
    let answer = prelogin(&[VERSION, (0x01, &[NOT_SUP]), (0x02, &[1])]);
    let (res, _) = connect(answer, EncryptionLevel::NotSupported, Some("INST")).await;
    match res {
        Err(Error::Protocol(msg)) => assert!(msg.contains("rejected"), "{msg}"),
        other => panic!("expected the instance to be rejected, got {other:?}"),
    }
}

/// ENCRYPT_CLIENT_CERT (0x80, MS-TDS 2.2.6.5) is decoded instead of
/// failing the prelogin, and the TLS handshake follows as for its low bits.
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
#[tokio::test]
async fn client_cert_bit_is_decoded() {
    for (client, server) in [
        (EncryptionLevel::Off, 0x80 | OFF),
        (EncryptionLevel::Off, 0x80 | REQ),
        (EncryptionLevel::Required, 0x80 | ON),
    ] {
        let (res, seen) = connect(encryption(server), client, None).await;
        // The fake server doesn't do TLS.
        assert!(res.is_err(), "{client:?}/{server:#x}");
        assert_eq!(seen, Some(0x12), "{client:?}/{server:#x}: TLS handshake");
    }

    // Certificate authentication without TLS ends the connection.
    for client in [EncryptionLevel::NotSupported, EncryptionLevel::Off] {
        let (res, seen) = connect(encryption(0x80 | NOT_SUP), client, None).await;
        match res {
            Err(Error::Protocol(msg)) => assert!(!msg.contains("invalid"), "{client:?}: {msg}"),
            other => panic!("{client:?}: {other:?}"),
        }
        assert_eq!(seen, None, "{client:?}");
    }
}

#[cfg(all(windows, feature = "winauth"))]
fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// An NTLM CHALLENGE message (MS-NLMP 2.2.1.2) in an SSPI token.
#[cfg(all(windows, feature = "winauth"))]
fn ntlm_challenge() -> Vec<u8> {
    let name = utf16("fake");
    // MsvAvDnsComputerName, then MsvAvEOL.
    let mut info = vec![3, 0];
    info.extend_from_slice(&(name.len() as u16).to_le_bytes());
    info.extend(name);
    info.extend_from_slice(&[0, 0, 0, 0]);
    let len = (info.len() as u16).to_le_bytes();

    let mut m = b"NTLMSSP\0".to_vec();
    m.extend_from_slice(&2u32.to_le_bytes());
    // An empty TargetName after TargetInfo.
    m.extend_from_slice(&[0, 0, 0, 0]);
    m.extend_from_slice(&(48 + info.len() as u32).to_le_bytes());
    // KEY_EXCH, 128, TARGET_INFO, EXTENDED_SESSIONSECURITY, ALWAYS_SIGN,
    // SEAL, UNICODE.
    m.extend_from_slice(&0x6088_8021u32.to_le_bytes());
    m.extend_from_slice(&[7; 8]);
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&[len[0], len[1], len[0], len[1]]);
    m.extend_from_slice(&48u32.to_le_bytes());
    m.extend(info);

    let mut token = vec![0xED];
    token.extend_from_slice(&(m.len() as u16).to_le_bytes());
    token.extend(m);
    token
}

/// The SQL Browser's port stays with the caller's stream, so the SPN of a
/// named instance names the instance, as SqlClient does when it has no port,
/// instead of carrying the browser's port 1434. NTLMv2 sends the SPN in its
/// AUTHENTICATE message (MsvAvTargetName).
#[cfg(all(windows, feature = "winauth"))]
#[tokio::test]
async fn spn_of_a_named_instance_without_a_port_names_the_instance() {
    let mut config = Config::new();
    config.host("127.0.0.1");
    config.instance_name("INST");
    config.encryption(EncryptionLevel::NotSupported);
    config.authentication(AuthMethod::windows(r"DOMAIN\user", "pw"));

    let has = ntlm_authenticate(config).await;
    assert!(has("MSSQLSvc/127.0.0.1:INST"));
    assert!(!has("MSSQLSvc/127.0.0.1:1434"));
}

/// A non-blank `Server SPN` (`serverSpn` in JDBC) is the SPN as given, as
/// in SqlClient; it was an error with integrated security.
#[cfg(all(windows, feature = "winauth"))]
#[tokio::test]
async fn server_spn_is_used_as_given() {
    let spn = "MSSQLSvc/db.example.com:1433";
    for s in [
        format!(
            r"Server=tcp:127.0.0.1\INST;Integrated Security=true;User ID=DOMAIN\user;Password=pw;Encrypt=DANGER_PLAINTEXT;Server SPN={spn}"
        ),
        format!(
            r"Server=127.0.0.1\INST;Trusted_Connection=yes;UID=DOMAIN\user;PWD=pw;Encrypt=DANGER_PLAINTEXT;ServerSPN={spn}"
        ),
    ] {
        let has = ntlm_authenticate(Config::from_ado_string(&s).unwrap()).await;
        assert!(has(spn), "{s}");
        assert!(!has("MSSQLSvc/127.0.0.1:INST"), "{s}");
    }

    let s = format!(
        r"jdbc:sqlserver://127.0.0.1\INST;integratedSecurity=true;user=DOMAIN\user;password=pw;encrypt=DANGER_PLAINTEXT;serverSpn={spn}"
    );
    let has = ntlm_authenticate(Config::from_jdbc_string(&s).unwrap()).await;
    assert!(has(spn));

    // A blank one is no SPN, as SqlClient's IsNullOrWhiteSpace has it.
    let s = r"Server=127.0.0.1\INST;Integrated Security=true;User ID=DOMAIN\user;Password=pw;Encrypt=DANGER_PLAINTEXT;Server SPN=' '";
    let has = ntlm_authenticate(Config::from_ado_string(s).unwrap()).await;
    assert!(has("MSSQLSvc/127.0.0.1:INST"));
}

/// Connects with `config` to a fake server that answers LOGIN7 with an NTLM
/// challenge; tells whether the AUTHENTICATE message names an SPN.
#[cfg(all(windows, feature = "winauth"))]
async fn ntlm_authenticate(config: Config) -> impl Fn(&str) -> bool {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        read_message(&mut s).await.unwrap();
        let answer = prelogin(&[VERSION, (0x01, &[NOT_SUP]), (0x02, &[0])]);
        s.write_all(&packet(4, &answer)).await.unwrap();
        read_message(&mut s).await.unwrap();
        s.write_all(&packet(4, &ntlm_challenge())).await.unwrap();
        read_message_body(&mut s).await.unwrap().1
    });

    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let connect = Client::connect(config, tcp.compat_write());
    let _ = tokio::time::timeout(Duration::from_secs(10), connect).await;

    let authenticate = server.await.unwrap();
    move |spn: &str| authenticate.windows(spn.len() * 2).any(|w| w == utf16(spn))
}
