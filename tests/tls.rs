//! TLS against in-process fake servers using `tests/certs/server.pfx`
//! (`docker/certs/server.crt`, CN=server, SAN=localhost, issued by
//! `docker/certs/customCA.crt`).
#![cfg(feature = "native-tls")]

use async_native_tls::TlsAcceptor;
use futures_util::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tiberius::{Client, Config};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

const CA: &str = "docker/certs/customCA.crt";

fn packet(ty: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 8) as u16;
    let mut p = vec![ty, 1, (len >> 8) as u8, len as u8, 0, 0, 1, 0];
    p.extend_from_slice(payload);
    p
}

/// PRELOGIN answer: VERSION, ENCRYPTION, INSTOPT, THREADID, MARS.
fn prelogin_response(encryption: u8) -> Vec<u8> {
    let mut p = vec![
        0x00, 0, 26, 0, 6, 0x01, 0, 32, 0, 1, 0x02, 0, 33, 0, 1, 0x03, 0, 34, 0, 0, 0x04, 0, 34, 0,
        1, 0xff,
    ];
    p.extend_from_slice(&[16, 0, 0x07, 0xd0, 0, 0, encryption, 0, 0]);
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

async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    loop {
        let mut h = [0u8; 8];
        r.read_exact(&mut h).await?;
        let len = u16::from_be_bytes([h[2], h[3]]) as usize;
        let mut b = vec![0u8; len - 8];
        r.read_exact(&mut b).await?;
        out.extend(b);
        if h[1] & 1 == 1 {
            return Ok((h[0], out));
        }
    }
}

/// The server side of TDS 7.x's TLS-in-PRELOGIN framing.
struct ServerPrelogin<S> {
    s: S,
    wrapping: bool,
    hdr: [u8; 8],
    hpos: usize,
    remaining: usize,
    out: Vec<u8>,
}

impl<S: AsyncWrite + Unpin> ServerPrelogin<S> {
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while !self.out.is_empty() {
            let n = futures_util::ready!(Pin::new(&mut self.s).poll_write(cx, &self.out))?;
            self.out.drain(..n);
        }
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ServerPrelogin<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        if !this.wrapping {
            return Pin::new(&mut this.s).poll_read(cx, buf);
        }
        if this.remaining == 0 {
            while this.hpos < 8 {
                let n = futures_util::ready!(
                    Pin::new(&mut this.s).poll_read(cx, &mut this.hdr[this.hpos..])
                )?;
                if n == 0 {
                    return Poll::Ready(Ok(0));
                }
                this.hpos += n;
            }
            this.remaining = u16::from_be_bytes([this.hdr[2], this.hdr[3]]) as usize - 8;
            this.hpos = 0;
        }
        let max = buf.len().min(this.remaining);
        let n = futures_util::ready!(Pin::new(&mut this.s).poll_read(cx, &mut buf[..max]))?;
        this.remaining -= n;
        Poll::Ready(Ok(n))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ServerPrelogin<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        futures_util::ready!(this.drain(cx))?;
        if !this.wrapping {
            return Pin::new(&mut this.s).poll_write(cx, buf);
        }
        this.out.extend(packet(0x12, buf));
        let _ = this.drain(cx)?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        futures_util::ready!(self.drain(cx))?;
        Pin::new(&mut self.s).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.s).poll_close(cx)
    }
}

/// Serves one TLS connection: TDS 8.0 strict, or TDS 7.x full encryption.
async fn fake_server(strict: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut raw = tcp.compat();
        let pfx = std::fs::read("tests/certs/server.pfx").unwrap();
        let acceptor = TlsAcceptor::new(futures_util::io::Cursor::new(pfx), "tiberius")
            .await
            .unwrap();

        let mut tls = if strict {
            let Ok(tls) = acceptor
                .accept(ServerPrelogin {
                    s: raw,
                    wrapping: false,
                    hdr: [0; 8],
                    hpos: 0,
                    remaining: 0,
                    out: Vec::new(),
                })
                .await
            else {
                return;
            };
            tls
        } else {
            read_message(&mut raw).await.unwrap();
            raw.write_all(&packet(4, &prelogin_response(1)))
                .await
                .unwrap();
            let wrapped = ServerPrelogin {
                s: raw,
                wrapping: true,
                hdr: [0; 8],
                hpos: 0,
                remaining: 0,
                out: Vec::new(),
            };
            let Ok(mut tls) = acceptor.accept(wrapped).await else {
                return;
            };
            tls.get_mut().wrapping = false;
            tls
        };

        if strict {
            let Ok(_) = read_message(&mut tls).await else {
                return;
            };
            tls.write_all(&packet(4, &prelogin_response(0)))
                .await
                .unwrap();
            tls.flush().await.unwrap();
        }

        let Ok((0x10, _)) = read_message(&mut tls).await else {
            return;
        };
        tls.write_all(&packet(4, &login_response())).await.unwrap();
        tls.flush().await.unwrap();
        // Keep the connection open until the client has read the reply.
        let _ = tls.read(&mut [0u8; 1]).await;
    });

    port
}

async fn connect(port: u16, connection_string: &str) -> tiberius::Result<()> {
    let cs = format!("server=tcp:localhost,{port};user=sa;password=x;{connection_string}");
    let config = Config::from_ado_string(&cs)?;
    let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect(config, tcp.compat_write()),
    )
    .await
    .expect("connect timed out")
    .map(drop)
}

/// TLS 1.3 sends post-handshake messages the TDS 7.x framing cannot carry, so
/// the client must cap the version at TLS 1.2 for a server that offers 1.3.
#[tokio::test]
async fn tds7_tls_handshake_against_a_tls13_capable_server() -> tiberius::Result<()> {
    let port = fake_server(false).await;
    connect(port, &format!("encrypt=true;TrustServerCertificateCA={CA}")).await
}

#[cfg(feature = "tds80")]
#[tokio::test]
async fn strict_ignores_trust_server_certificate() {
    // Issued by a CA the system does not trust: validation must fail.
    let port = fake_server(true).await;
    let res = connect(port, "encrypt=strict;TrustServerCertificate=true").await;
    assert!(
        matches!(res, Err(tiberius::error::Error::Tls(_))),
        "{res:?}"
    );
}

#[cfg(feature = "tds80")]
#[tokio::test]
async fn strict_with_trust_server_certificate_honours_the_ca() -> tiberius::Result<()> {
    let port = fake_server(true).await;
    connect(
        port,
        &format!("encrypt=strict;TrustServerCertificate=true;TrustServerCertificateCA={CA}"),
    )
    .await
}

#[cfg(feature = "tds80")]
#[tokio::test]
async fn strict_ignores_trust_cert_set_through_the_api() {
    let port = fake_server(true).await;
    let mut config = Config::new();
    config.host("localhost");
    config.port(port);
    config.authentication(tiberius::AuthMethod::sql_server("sa", "x"));
    config.trust_cert();
    config.encryption(tiberius::EncryptionLevel::Strict);
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let res = Client::connect(config, tcp.compat_write()).await.map(drop);
    assert!(
        matches!(res, Err(tiberius::error::Error::Tls(_))),
        "{res:?}"
    );
}
