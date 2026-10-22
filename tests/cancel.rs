//! Integration tests for `Client::cancel_query` and `CancelHandle`.

use futures_util::future;
use futures_util::io::{AsyncRead, AsyncWrite};
use futures_util::FutureExt;
use futures_util::TryStreamExt;
use once_cell::sync::Lazy;
use std::env;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Once, OnceLock};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use runtimes_macro::test_on_runtimes;
use tds_driver::{CancelHandle, Client, Config, Result};

// This is used in the testing macro :)
#[allow(dead_code)]
static LOGGER_SETUP: Once = Once::new();

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TDS_DRIVER_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;user=SA;password=<YourStrong@Passw0rd>;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

/// A cancelled request's result. A server that takes the Attention before the
/// request runs reports it aborted (error 3980) rather than returning nothing.
fn cancelled<T: Default>(result: Result<T>) -> Result<T> {
    match result {
        Err(tds_driver::error::Error::Server(e)) if e.code() == 3980 => Ok(T::default()),
        result => result,
    }
}

#[test_on_runtimes]
async fn cancel_mid_stream<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let stream = conn
        .simple_query(
            "CREATE TABLE #cancelled (id INT); \
             INSERT INTO #cancelled VALUES (1); \
             SELECT 1 FROM sys.all_columns a CROSS JOIN sys.all_columns b; \
             INSERT INTO #cancelled VALUES (2);",
        )
        .await?;
    drop(stream);
    conn.cancel_query().await?;

    let ids: Vec<Option<i32>> = conn
        .query("SELECT id FROM #cancelled", &[])
        .await?
        .into_first_result()
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();

    // The second INSERT never ran, and the query read its own result.
    assert_eq!(vec![Some(1)], ids);

    Ok(())
}

#[test_on_runtimes]
async fn cancel_after_a_server_error<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // `simple_query` returns once NOWAIT flushes the output, error included.
    let stream = conn
        .simple_query(
            "SELECT 1; \
             RAISERROR('discarded', 16, 1) WITH NOWAIT; \
             WAITFOR DELAY '00:00:05';",
        )
        .await?;
    drop(stream);
    conn.cancel_query().await?;

    let row = conn
        .query("SELECT @P1", &[&42i32])
        .await?
        .into_row()
        .await?
        .unwrap();

    assert_eq!(Some(42i32), row.get(0));

    Ok(())
}

#[test_on_runtimes]
async fn cancel_query_dropped_before_the_acknowledgement<S>(
    mut conn: tds_driver::Client<S>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // `simple_query` returns once NOWAIT flushes the first result.
    let stream = conn
        .simple_query(
            "SELECT 1; \
             RAISERROR('', 0, 1) WITH NOWAIT; \
             WAITFOR DELAY '00:00:05';",
        )
        .await?;
    drop(stream);
    // Polled once, as under a timeout: the Attention goes out, and the future
    // is dropped waiting for the acknowledgement, which the next request reads.
    let _ = conn.cancel_query().now_or_never();

    for i in 0..2i32 {
        let row = conn
            .query("SELECT @P1", &[&i])
            .await?
            .into_row()
            .await?
            .unwrap();

        assert_eq!(Some(i), row.get(0));
    }

    Ok(())
}

#[test_on_runtimes]
async fn cancel_handle_before_the_server_answers<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let handle = conn.cancel_handle();

    // Twice, as the second cancel must not be mistaken for the first.
    for i in 0..2i32 {
        // From another thread, while the server is in the WAITFOR and has
        // nothing to send.
        let cancel = handle.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let results = async {
            conn.query("WAITFOR DELAY '00:00:30'; SELECT @P1", &[&i])
                .await?
                .into_results()
                .await
        }
        .await;

        // The SELECT never ran.
        assert!(results?.is_empty());

        let row = conn
            .query("SELECT @P1", &[&42i32])
            .await?
            .into_row()
            .await?
            .unwrap();

        assert_eq!(Some(42i32), row.get(0));
    }

    Ok(())
}

#[test_on_runtimes]
async fn cancel_handle_with_no_request_in_flight<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    conn.cancel_handle().cancel();

    let row = conn
        .query("WAITFOR DELAY '00:00:01'; SELECT @P1", &[&42i32])
        .await?
        .into_row()
        .await?
        .unwrap();

    // The cancel was not for this request.
    assert_eq!(Some(42i32), row.get(0));

    Ok(())
}

#[test_on_runtimes]
async fn cancel_handle_racing_the_response<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let handle = conn.cancel_handle();

    // A server that finishes before it sees the Attention acknowledges it in a
    // message of its own, which the next request has to read past. Which comes
    // first is down to timing, so race them repeatedly.
    for i in 0..50i32 {
        let query = async {
            conn.query("SELECT @P1", &[&i])
                .await?
                .into_first_result()
                .await
        };
        let (rows, ()) = future::join(query, async { handle.cancel() }).await;
        assert!(cancelled(rows)?.len() <= 1);

        let row = conn
            .query("SELECT @P1", &[&-i])
            .await?
            .into_row()
            .await?
            .unwrap();

        assert_eq!(Some(-i), row.get(0));
    }

    Ok(())
}

#[test_on_runtimes]
async fn cancel_query_after_cancel_handle<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let handle = conn.cancel_handle();
    // `simple_query` returns once NOWAIT flushes the first result.
    let mut stream = conn
        .simple_query(
            "SELECT 1; \
             RAISERROR('', 0, 1) WITH NOWAIT; \
             WAITFOR DELAY '00:00:30';",
        )
        .await?;
    handle.cancel();

    // Read on until the stream waits for the server, which sends the
    // Attention, then drop it before the acknowledgement arrives.
    loop {
        let next = stream.try_next();
        futures_util::pin_mut!(next);
        if let future::Either::Right(_) = future::select(next, future::ready(())).await {
            break;
        }
    }
    drop(stream);
    conn.cancel_query().await?;

    let row = conn
        .query("SELECT @P1", &[&42i32])
        .await?
        .into_row()
        .await?
        .unwrap();

    assert_eq!(Some(42i32), row.get(0));

    Ok(())
}

#[test_on_runtimes]
async fn cancel_handle_then_dropped_response<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let handle = conn.cancel_handle();
    let stream = conn
        .simple_query(
            "SELECT 1; \
             RAISERROR('', 0, 1) WITH NOWAIT; \
             WAITFOR DELAY '00:00:01';",
        )
        .await?;
    handle.cancel();
    // The next request discards the rest of the response without sending the
    // Attention, which could otherwise be acknowledged in what it discards.
    drop(stream);

    for i in 0..2i32 {
        let row = conn
            .query("SELECT @P1", &[&i])
            .await?
            .into_row()
            .await?
            .unwrap();

        assert_eq!(Some(i), row.get(0));
    }

    Ok(())
}

/// A transport that cancels as it writes a request containing `sql`, so the
/// cancel lands while the client waits for the answer.
struct CancelOnWrite {
    inner: Compat<TcpStream>,
    sql: Vec<u8>,
    cancel: Arc<OnceLock<CancelHandle>>,
}

impl AsyncRead for CancelOnWrite {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for CancelOnWrite {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.windows(self.sql.len()).any(|w| w == self.sql) {
            if let Some(cancel) = self.cancel.get() {
                cancel.cancel();
            }
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}

#[tokio::test]
async fn cancel_handle_stops_a_bulk_insert_before_the_rows() -> Result<()> {
    // The two requests a bulk insert makes before the rows.
    for sql in ["SELECT TOP 0", "INSERT BULK"] {
        // Unencrypted after login, so the transport can see the SQL.
        let config = Config::from_ado_string(&format!("{};encrypt=false", *CONN_STR))?;
        let tcp = TcpStream::connect(config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        let cancel = Arc::new(OnceLock::new());
        let transport = CancelOnWrite {
            inner: tcp.compat_write(),
            sql: sql.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            cancel: Arc::clone(&cancel),
        };
        let mut conn = Client::connect(config, transport).await?;
        cancel.get_or_init(|| conn.cancel_handle());

        conn.simple_query("CREATE TABLE #bulk (id INT)")
            .await?
            .into_results()
            .await?;
        assert!(conn.bulk_insert("#bulk").await.is_err(), "{sql}");

        let row = conn
            .query("SELECT COUNT(*) FROM #bulk", &[])
            .await?
            .into_row()
            .await?
            .unwrap();

        assert_eq!(Some(0i32), row.get(0));
    }

    Ok(())
}
