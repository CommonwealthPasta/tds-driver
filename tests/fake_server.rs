//! Responses SQL Server doesn't send, from an in-process fake server.

use std::time::Duration;
use tiberius::{error::Error, AuthMethod, Client, Config, EncryptionLevel, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

const DONE_ERROR: u16 = 0x0002;
const DONE_SRVERROR: u16 = 0x0100;
const TDS_7_4: u32 = 0x7400_0004;

fn packet(ty: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 8) as u16;
    let mut p = vec![ty, 1, (len >> 8) as u8, len as u8, 0, 0, 1, 0];
    p.extend_from_slice(payload);
    p
}

/// PRELOGIN answer: VERSION, then ENCRYPTION = ENCRYPT_NOT_SUP.
fn prelogin() -> Vec<u8> {
    let mut p = vec![0x00, 0, 11, 0, 6, 0x01, 0, 17, 0, 1, 0xff];
    p.extend_from_slice(&[16, 0, 0x07, 0xd0, 0, 0, 2]);
    p
}

fn b_varchar(s: &str) -> Vec<u8> {
    let mut v = vec![s.encode_utf16().count() as u8];
    v.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
    v
}

fn login_ack(tds_version: u32) -> Vec<u8> {
    let mut body = vec![1u8];
    body.extend_from_slice(&tds_version.to_be_bytes());
    body.extend(b_varchar("Fake"));
    body.extend_from_slice(&[16, 0, 0x07, 0xd0]);
    let mut t = vec![0xAD];
    t.extend_from_slice(&(body.len() as u16).to_le_bytes());
    t.extend(body);
    t
}

fn done(status: u16) -> Vec<u8> {
    let mut t = vec![0xFD];
    t.extend_from_slice(&status.to_le_bytes());
    t.extend_from_slice(&0xC1u16.to_le_bytes());
    t.extend_from_slice(&0u64.to_le_bytes());
    t
}

fn error(number: u32, message: &str) -> Vec<u8> {
    let mut body = number.to_le_bytes().to_vec();
    body.extend_from_slice(&[1, 16]);
    body.extend_from_slice(&(message.encode_utf16().count() as u16).to_le_bytes());
    body.extend(message.encode_utf16().flat_map(u16::to_le_bytes));
    body.extend(b_varchar("fake"));
    body.extend(b_varchar(""));
    body.extend_from_slice(&1u32.to_le_bytes());
    let mut t = vec![0xAA];
    t.extend_from_slice(&(body.len() as u16).to_le_bytes());
    t.extend(body);
    t
}

/// Reads one message.
async fn read_message(s: &mut TcpStream) -> std::io::Result<()> {
    loop {
        let mut h = [0u8; 8];
        s.read_exact(&mut h).await?;
        let mut body = vec![0u8; u16::from_be_bytes([h[2], h[3]]) as usize - 8];
        s.read_exact(&mut body).await?;
        if h[1] & 1 == 1 {
            return Ok(());
        }
    }
}

/// Answers PRELOGIN, then LOGIN7 with `login`, then each request with the
/// next of `responses`.
async fn fake_server(login: Vec<u8>, responses: Vec<Vec<u8>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        for answer in [prelogin(), login].into_iter().chain(responses) {
            if read_message(&mut s).await.is_err() {
                return;
            }
            s.write_all(&packet(4, &answer)).await.unwrap();
        }
        let _ = s.read(&mut [0u8; 1]).await;
    });

    port
}

async fn connect(login: Vec<u8>, responses: Vec<Vec<u8>>) -> Result<Client<Compat<TcpStream>>> {
    let port = fake_server(login, responses).await;
    let mut config = Config::new();
    config.host("127.0.0.1");
    config.port(port);
    config.encryption(EncryptionLevel::NotSupported);
    config.authentication(AuthMethod::sql_server("sa", "x"));

    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect(config, tcp.compat_write()),
    )
    .await
    .expect("connect timed out")
}

fn login(tds_version: u32, done_status: u16) -> Vec<u8> {
    [login_ack(tds_version), done(done_status)].concat()
}

/// SqlClient's `TdsParser.TryProcessDone` raises a class 11 error for
/// DONE_ERROR with no ERROR token, and a class 20 one for DONE_SRVERROR.
#[tokio::test]
async fn done_error_without_an_error_token_fails_the_request() -> Result<()> {
    let responses = vec![
        done(DONE_ERROR),
        done(DONE_SRVERROR),
        [error(50000, "boom"), done(DONE_ERROR)].concat(),
        done(0),
    ];
    let mut client = connect(login(TDS_7_4, 0), responses).await?;

    for (class, code) in [(11, 0), (20, 0), (16, 50000)] {
        match client.simple_query("SELECT 1").await {
            Err(Error::Server(e)) => {
                assert_eq!((e.class(), e.code()), (class, code), "{e}");
                assert!(e.other_errors().is_empty(), "{e:?}");
                if code == 0 {
                    assert!(e.message().contains("severe error"), "{e}");
                }
            }
            Err(e) => panic!("expected a server error, got {e:?}"),
            Ok(_) => panic!("class {class}: the request must fail"),
        }
    }

    let results = client
        .simple_query("SELECT 1")
        .await?
        .into_results()
        .await?;
    assert!(results.is_empty());

    Ok(())
}

/// The same check on the login response.
#[tokio::test]
async fn login_done_error_without_an_error_token_fails_the_connect() {
    match connect(login(TDS_7_4, DONE_ERROR), vec![]).await {
        Err(Error::Server(e)) => assert_eq!((e.class(), e.code()), (11, 0), "{e}"),
        Err(e) => panic!("expected a server error, got {e:?}"),
        Ok(_) => panic!("the connect must fail"),
    }
}
