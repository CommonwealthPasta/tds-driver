//! Integration tests for `Client::cancel_query`.

use futures_util::io::{AsyncRead, AsyncWrite};
use once_cell::sync::Lazy;
use std::env;
use std::sync::Once;

use runtimes_macro::test_on_runtimes;
use tiberius::Result;

// This is used in the testing macro :)
#[allow(dead_code)]
static LOGGER_SETUP: Once = Once::new();

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TIBERIUS_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;user=SA;password=<YourStrong@Passw0rd>;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

#[test_on_runtimes]
async fn cancel_mid_stream<S>(mut conn: tiberius::Client<S>) -> Result<()>
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
async fn cancel_after_a_server_error<S>(mut conn: tiberius::Client<S>) -> Result<()>
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
