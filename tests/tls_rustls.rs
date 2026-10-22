//! The rustls backend against an in-process rustls server using
//! `docker/certs/server.crt` (SAN=localhost, issued by
//! `docker/certs/customCA.crt`). Run with
//! `cargo test --no-default-features --features rustls,tds80 --test tls_rustls`.
#![cfg(all(feature = "rustls", feature = "tds80"))]

use std::{sync::Arc, time::Duration};
use tds_driver::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{
    rustls::{
        crypto::aws_lc_rs,
        pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
        version, ProtocolVersion, ServerConfig,
    },
    TlsAcceptor,
};
use tokio_util::compat::TokioAsyncWriteCompatExt;

fn packet(ty: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 8) as u16;
    let mut p = vec![ty, 1, (len >> 8) as u8, len as u8, 0, 0, 1, 0];
    p.extend_from_slice(payload);
    p
}

/// PRELOGIN answer: VERSION and ENCRYPTION, which strict mode ignores.
fn prelogin_response() -> Vec<u8> {
    let mut p = vec![0x00, 0, 11, 0, 6, 0x01, 0, 17, 0, 1, 0xff];
    p.extend_from_slice(&[16, 0, 0x07, 0xd0, 0, 0, 0]);
    p
}

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
async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<u8> {
    let mut ty = None;
    loop {
        let mut h = [0u8; 8];
        r.read_exact(&mut h).await?;
        let mut body = vec![0u8; u16::from_be_bytes([h[2], h[3]]) as usize - 8];
        r.read_exact(&mut body).await?;
        let ty = *ty.get_or_insert(h[0]);
        if h[1] & 1 == 1 {
            return Ok(ty);
        }
    }
}

/// Serves one TDS 8.0 strict connection over TLS 1.3 only; returns the port
/// and the negotiated version.
async fn tls13_only_server() -> (u16, tokio::task::JoinHandle<Option<ProtocolVersion>>) {
    let certs = CertificateDer::pem_file_iter("docker/certs/server.crt")
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file("docker/certs/server.key").unwrap();
    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_protocol_versions(&[&version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(tcp).await.ok()?;
        let version = tls.get_ref().1.protocol_version();
        read_message(&mut tls).await.ok()?;
        tls.write_all(&packet(4, &prelogin_response())).await.ok()?;
        if read_message(&mut tls).await.ok()? == 0x10 {
            tls.write_all(&packet(4, &login_response())).await.ok()?;
            tls.flush().await.ok()?;
            let _ = tls.read(&mut [0u8; 1]).await;
        }
        version
    });
    (port, server)
}

/// TDS 8.0 strict runs TLS straight over TCP, so TLS 1.3 works there; the
/// TLS 1.2 cap is for TDS 7.x only.
#[tokio::test]
async fn strict_negotiates_tls13() -> tds_driver::Result<()> {
    let (port, server) = tls13_only_server().await;
    let mut config = Config::new();
    config.host("localhost");
    config.port(port);
    config.encryption(EncryptionLevel::Strict);
    config.trust_cert_ca("docker/certs/customCA.crt");
    config.authentication(AuthMethod::sql_server("sa", "x"));

    let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
    let client = tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect(config, tcp.compat_write()),
    )
    .await
    .expect("connect timed out")?;
    drop(client);

    assert_eq!(server.await.unwrap(), Some(ProtocolVersion::TLSv1_3));
    Ok(())
}
