//! Connection setup against the test server: prelogin, TLS and login.

use once_cell::sync::Lazy;
use std::env;
use tds_driver::{error::Error, Client, Config, Result};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
use {
    futures_util::io::{AsyncRead, AsyncWrite},
    std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    },
    tds_driver::{AuthMethod, EncryptionLevel},
};

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TDS_DRIVER_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;user=SA;password=<YourStrong@Passw0rd>;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

fn config() -> Config {
    Config::from_ado_string(&CONN_STR).unwrap()
}

async fn connect(config: Config) -> Result<Client<Compat<TcpStream>>> {
    let tcp = TcpStream::connect(config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Client::connect(config, tcp.compat_write()).await
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
async fn encrypt_option<S: AsyncRead + AsyncWrite + Unpin + Send>(
    client: &mut Client<S>,
) -> Result<String> {
    let row = client
        .simple_query(
            "SELECT CAST(encrypt_option AS nvarchar(10)) FROM sys.dm_exec_connections \
             WHERE session_id = @@SPID",
        )
        .await?
        .into_row()
        .await?
        .unwrap();
    Ok(row.get::<&str, _>(0).unwrap().to_owned())
}

/// Under login-only encryption only the first packet of a multi-packet
/// LOGIN7 is encrypted; the server drops the connection if all of it is.
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
#[tokio::test]
async fn multi_packet_login_with_login_only_encryption() -> Result<()> {
    for level in [EncryptionLevel::Off, EncryptionLevel::Required] {
        let mut config = config();
        config.encryption(level);
        // A 12 KB token makes LOGIN7 span three 4 KB packets. The server has no
        // Entra ID, so it answers with a login error once it has read it all.
        config.authentication(AuthMethod::aad_token(format!("eyJ{}", "a".repeat(6000))));

        match connect(config).await {
            Err(Error::Server(e)) if e.code() == 18456 => (),
            other => panic!("{level:?}: expected login failure 18456, got {other:?}"),
        }
    }
    Ok(())
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
#[tokio::test]
async fn login_only_encryption_leaves_the_session_unencrypted() -> Result<()> {
    let mut config = config();
    config.encryption(EncryptionLevel::Off);
    let mut client = connect(config).await?;
    assert_eq!(encrypt_option(&mut client).await?, "FALSE");
    Ok(())
}

/// An explicit port wins over the instance name.
#[tokio::test]
async fn instance_name_with_explicit_port() -> Result<()> {
    let mut config = config();
    config.instance_name("NOSUCHINSTANCE");
    assert!(config.get_addr().ends_with(":1433"));
    let mut client = connect(config).await?;
    client.simple_query("SELECT 1").await?.into_row().await?;
    Ok(())
}

#[cfg(feature = "sql-browser-tokio")]
#[tokio::test]
async fn connect_named_with_explicit_port_skips_the_browser() -> Result<()> {
    use tds_driver::SqlBrowser;

    let mut config = config();
    config.instance_name("NOSUCHINSTANCE");
    let tcp = TcpStream::connect_named(&config).await?;
    let mut client = Client::connect(config, tcp.compat_write()).await?;
    client.simple_query("SELECT 1").await?.into_row().await?;
    Ok(())
}

/// SqlClient's synonyms reach the server; the last of them wins.
#[tokio::test]
async fn sqlclient_synonyms_reach_the_server() -> Result<()> {
    let addr = config().get_addr();
    let (host, port) = addr.rsplit_once(':').unwrap();
    let cs = format!(
        "{};server=tcp:nosuchhost,1;Addr=TCP: {host} , {port} ;\
         Application Name=x;App=syn-app;WSID=syn-ws;Packet Size=2048;Pooling=false",
        *CONN_STR
    );
    let session = |config: Config| async {
        let mut client = connect(config).await?;
        let row = client
            .simple_query(
                "SELECT APP_NAME(), HOST_NAME(), net_packet_size FROM sys.dm_exec_connections \
                 WHERE session_id = @@SPID",
            )
            .await?
            .into_row()
            .await?
            .unwrap();
        let text = |i| row.get::<&str, _>(i).map(str::to_owned);
        Result::Ok((text(0), text(1), row.get::<i32, _>(2)))
    };

    let (app, ws, size) = session(Config::from_ado_string(&cs)?).await?;
    assert_eq!(app.as_deref(), Some("syn-app"));
    assert_eq!(ws.as_deref(), Some("syn-ws"));
    // The server reports the size with its TLS overhead, so compare with the
    // same size set through the API.
    let mut sized = config();
    sized.packet_size(2048);
    assert_eq!(size, session(sized).await?.2);
    Ok(())
}

#[tokio::test]
async fn overlong_login_field_is_a_clear_error() {
    let mut config = config();
    config.application_name("a".repeat(129));
    match connect(config).await {
        Err(Error::Conversion(msg)) => {
            assert!(
                msg.contains("application name") && msg.contains("128"),
                "{msg}"
            )
        }
        other => panic!("expected a conversion error, got {:?}", other.map(drop)),
    }
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
/// Accepts at most `max` bytes per read or write, and returns `Pending` on
/// every other call.
struct Choppy<S> {
    inner: S,
    max: usize,
    flip_read: bool,
    flip_write: bool,
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
fn pending(flip: &mut bool, cx: &mut Context<'_>) -> bool {
    *flip = !*flip;
    if *flip {
        cx.waker().wake_by_ref();
    }
    *flip
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
impl<S: AsyncRead + Unpin> AsyncRead for Choppy<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        if pending(&mut this.flip_read, cx) {
            return Poll::Pending;
        }
        let n = buf.len().min(this.max);
        Pin::new(&mut this.inner).poll_read(cx, &mut buf[..n])
    }
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
impl<S: AsyncWrite + Unpin> AsyncWrite for Choppy<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        if pending(&mut this.flip_write, cx) {
            return Poll::Pending;
        }
        let n = buf.len().min(this.max);
        Pin::new(&mut this.inner).poll_write(cx, &buf[..n])
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

/// The TLS handshake survives partial writes and `Pending` on the transport.
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
#[tokio::test]
async fn tls_handshake_over_a_choppy_transport() -> Result<()> {
    for max in [1, 3, 7] {
        let mut config = config();
        config.encryption(EncryptionLevel::Required);
        let tcp = TcpStream::connect(config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        let choppy = Choppy {
            inner: tcp.compat_write(),
            max,
            flip_read: false,
            flip_write: false,
        };
        let mut client = Client::connect(config, choppy).await?;
        assert_eq!(encrypt_option(&mut client).await?, "TRUE", "max {max}");
    }
    Ok(())
}
