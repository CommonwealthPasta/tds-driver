//! Integration tests for requests whose response is left partly unread, or
//! that fail before they are sent.

use futures_util::io::{AsyncRead, AsyncWrite};
use futures_util::task::AtomicWaker;
use futures_util::TryStreamExt;
use once_cell::sync::Lazy;
use std::env;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use runtimes_macro::test_on_runtimes;
use tds_driver::{Client, Command, Config, QueryItem, Result, SqlTableDataRow, TableValueRow};

// This is used in the testing macro :)
#[allow(dead_code)]
static LOGGER_SETUP: Once = Once::new();

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TDS_DRIVER_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;user=SA;password=<YourStrong@Passw0rd>;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

async fn trancount<S>(conn: &mut Client<S>) -> Result<i32>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let row = conn
        .simple_query("SELECT @@TRANCOUNT")
        .await?
        .into_row()
        .await?
        .unwrap();
    Ok(row.get(0).unwrap())
}

/// Reads up to and including the first row, then drops the stream.
async fn read_one_row<S>(conn: &mut Client<S>, sql: &str) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut stream = conn.simple_query(sql).await?;
    while let Some(item) = stream.try_next().await? {
        if let QueryItem::Row(_) = item {
            return Ok(());
        }
    }
    panic!("no row");
}

#[test_on_runtimes]
async fn a_transaction_change_in_an_unread_response_is_kept<S>(mut conn: Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // Left unread, the BEGIN's descriptor would be lost and the server would
    // reject the next request (3989); after the COMMIT, it would keep the
    // ended transaction's and reject every request (3971).
    read_one_row(&mut conn, "SELECT 1 AS a; BEGIN TRAN; SELECT 2 AS b").await?;
    assert_eq!(1, trancount(&mut conn).await?);

    read_one_row(&mut conn, "SELECT 1 AS a; COMMIT; SELECT 2 AS b").await?;
    assert_eq!(0, trancount(&mut conn).await?);

    Ok(())
}

struct Text(&'static str);

impl<'a> TableValueRow<'a> for Text {
    fn bind_fields(&self, row: &mut SqlTableDataRow<'a>) {
        row.add_field(self.0);
    }

    // Bound with an explicit type instead.
    fn get_db_type() -> &'static str {
        ""
    }
}

#[tokio::test]
async fn a_request_that_fails_to_encode_leaves_the_connection_usable() -> Result<()> {
    let (mut conn, _) = throttled().await?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (ty, proc) = (format!("unread_t{id}"), format!("unread_p{id}"));
    conn.simple_query(format!("CREATE TYPE {ty} AS TABLE (s varchar(4))"))
        .await?
        .into_results()
        .await?;
    conn.simple_query(format!(
        "CREATE PROCEDURE {proc} @p {ty} READONLY AS SELECT COUNT(*) FROM @p"
    ))
    .await?
    .into_results()
    .await?;

    // Too long for varchar(4), which only the encoder checks.
    let mut cmd = Command::new(&proc);
    cmd.bind_table_with_dbtype("@p", &ty, vec![Text("too long")]);
    let sent = cmd.exec(&mut conn).await.map(|_| ());
    assert!(sent.is_err());

    // Before, this waited forever for a response to the request never sent.
    let next = tokio::time::timeout(Duration::from_secs(10), trancount(&mut conn)).await;
    assert_eq!(0, next.expect("the connection hung")?);

    conn.simple_query(format!("DROP PROCEDURE {proc}; DROP TYPE {ty}"))
        .await?
        .into_results()
        .await?;

    Ok(())
}

/// Lets through at most `bytes` more of the response, then stalls until
/// given more.
#[derive(Default)]
struct Allowance {
    bytes: AtomicUsize,
    waker: AtomicWaker,
}

impl Allowance {
    fn set(&self, bytes: usize) {
        self.bytes.store(bytes, Ordering::SeqCst);
        self.waker.wake();
    }
}

struct Throttled {
    inner: Compat<TcpStream>,
    allowance: Arc<Allowance>,
}

impl AsyncRead for Throttled {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.allowance.waker.register(cx.waker());
        let allowed = self.allowance.bytes.load(Ordering::SeqCst);
        if allowed == 0 {
            return Poll::Pending;
        }

        let n = buf.len().min(allowed);
        let read = Pin::new(&mut self.inner).poll_read(cx, &mut buf[..n]);
        if let Poll::Ready(Ok(n)) = read {
            self.allowance.bytes.fetch_sub(n, Ordering::SeqCst);
        }
        read
    }
}

impl AsyncWrite for Throttled {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

async fn throttled() -> Result<(Client<Throttled>, Arc<Allowance>)> {
    // Unencrypted after login, so a TDS packet is a run of bytes on the wire.
    let mut config = Config::from_ado_string(&format!("{};encrypt=false", *CONN_STR))?;
    config.packet_size(4096);
    let tcp = TcpStream::connect(config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    let allowance = Arc::new(Allowance::default());
    allowance.set(usize::MAX);
    let transport = Throttled {
        inner: tcp.compat_write(),
        allowance: Arc::clone(&allowance),
    };
    Ok((Client::connect(config, transport).await?, allowance))
}

/// Starts `sql`, whose first column is a large nvarchar(max), lets `first`
/// bytes of the response through, and drops the stream partway through the
/// first row.
async fn abandon_first_row(
    conn: &mut Client<Throttled>,
    allowance: &Allowance,
    sql: &str,
    first: usize,
) -> Result<()> {
    allowance.set(first);
    let mut stream = conn.simple_query(sql).await?;
    assert!(matches!(
        stream.try_next().await?,
        Some(QueryItem::Metadata(_))
    ));
    let row = tokio::time::timeout(Duration::from_millis(500), stream.try_next()).await;
    assert!(
        row.is_err(),
        "the row should be waiting for its next packet"
    );
    drop(stream);
    allowance.set(usize::MAX);
    Ok(())
}

#[tokio::test]
async fn a_token_abandoned_partway_is_read_again_from_its_start() -> Result<()> {
    let (mut conn, allowance) = throttled().await?;
    let big = "SELECT REPLICATE(CAST(N'x' AS nvarchar(max)), 20000) AS big";

    // One packet: the column metadata and the start of the row.
    abandon_first_row(&mut conn, &allowance, &format!("{big}; BEGIN TRAN"), 4096).await?;
    assert_eq!(1, trancount(&mut conn).await?);

    abandon_first_row(&mut conn, &allowance, &format!("{big}; COMMIT"), 4096).await?;
    assert_eq!(0, trancount(&mut conn).await?);

    // Read again to the Attention's acknowledgement, too.
    let wait = format!("{big}; WAITFOR DELAY '00:00:05'");
    abandon_first_row(&mut conn, &allowance, &wait, 4096).await?;
    tokio::time::timeout(Duration::from_secs(3), conn.cancel_query())
        .await
        .expect("the cancel waited for the batch to finish")?;
    assert_eq!(0, trancount(&mut conn).await?);

    Ok(())
}

#[tokio::test]
async fn a_token_too_large_to_read_again_is_skipped() -> Result<()> {
    let (mut conn, allowance) = throttled().await?;

    // 4 MB, abandoned 2 MB in: past the 1 MiB a token keeps.
    abandon_first_row(
        &mut conn,
        &allowance,
        "SELECT REPLICATE(CAST(N'x' AS nvarchar(max)), 2000000) AS big; SELECT 1 AS n",
        2_000_000,
    )
    .await?;
    assert_eq!(0, trancount(&mut conn).await?);

    Ok(())
}
