//! PRELOGIN negotiation against an in-process fake server.

use std::time::Duration;
use tiberius::{error::Error, AuthMethod, Client, Config, EncryptionLevel};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_util::compat::TokioAsyncWriteCompatExt;

const OFF: u8 = 0;
const NOT_SUP: u8 = 2;
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
    let mut ty = None;
    loop {
        let mut h = [0u8; 8];
        s.read_exact(&mut h).await?;
        let mut body = vec![0u8; u16::from_be_bytes([h[2], h[3]]) as usize - 8];
        s.read_exact(&mut body).await?;
        let ty = *ty.get_or_insert(h[0]);
        if h[1] & 1 == 1 {
            return Ok(ty);
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
        let _ = s.read(&mut [0u8; 1]).await;
    });

    (port, rx)
}

async fn connect(
    answer: Vec<u8>,
    encryption: EncryptionLevel,
    instance: Option<&str>,
) -> (tiberius::Result<()>, Option<u8>) {
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
        (EncryptionLevel::Off, NOT_SUP),
        (EncryptionLevel::NotSupported, OFF),
        (EncryptionLevel::NotSupported, NOT_SUP),
    ] {
        let (res, seen) = connect(encryption(server), client, None).await;
        assert!(res.is_ok(), "{client:?}/{server}: {res:?}");
        assert_eq!(seen, Some(0x10), "{client:?}/{server}: LOGIN7 in the clear");
    }
}

#[tokio::test]
async fn mismatched_encryption_fails_before_tls() {
    for (client, server) in [
        (EncryptionLevel::Required, NOT_SUP),
        (EncryptionLevel::Required, OFF),
        (EncryptionLevel::NotSupported, REQ),
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
