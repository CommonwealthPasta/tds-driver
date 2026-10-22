//! Value decoding against a live server.

use futures_util::io::{AsyncRead, AsyncWrite};
use once_cell::sync::Lazy;
use std::env;
use std::sync::Once;

use runtimes_macro::test_on_runtimes;
use tiberius::{Result, Row};

// This is used in the testing macro :)
#[allow(dead_code)]
static LOGGER_SETUP: Once = Once::new();

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TIBERIUS_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;user=SA;password=<YourStrong@Passw0rd>;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

async fn rows<S>(conn: &mut tiberius::Client<S>, sql: &str) -> Result<Vec<Row>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    conn.simple_query(sql).await?.into_first_result().await
}

#[test_on_runtimes]
async fn lone_utf16_surrogates_decode_lossily<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let r = rows(
        &mut conn,
        "SELECT N'a' + NCHAR(0xD800) + N'b', CAST(NCHAR(0xDC00) AS nvarchar(max)), \
         CAST(NCHAR(0xD800) AS nchar(2)), CAST(NCHAR(0xDC00) AS ntext), \
         CAST(NCHAR(0xD800) AS sql_variant), NCHAR(0xD83D) + NCHAR(0xDE00)",
    )
    .await?;
    assert_eq!(r[0].get::<&str, _>(0), Some("a\u{FFFD}b"));
    assert_eq!(r[0].get::<&str, _>(1), Some("\u{FFFD}"));
    assert_eq!(r[0].get::<&str, _>(2), Some("\u{FFFD} "));
    assert_eq!(r[0].get::<&str, _>(3), Some("\u{FFFD}"));
    assert_eq!(r[0].get::<&str, _>(4), Some("\u{FFFD}"));
    assert_eq!(r[0].get::<&str, _>(5), Some("\u{1F600}"));

    // Column name.
    let r = rows(
        &mut conn,
        "DECLARE @s nvarchar(100) = N'SELECT 1 AS ' + QUOTENAME(N'c' + NCHAR(0xD800)); EXEC(@s)",
    )
    .await?;
    assert_eq!(r[0].columns()[0].name(), "c\u{FFFD}");

    // INFO message ahead of a result, then an ERROR message.
    let r = rows(
        &mut conn,
        "DECLARE @m nvarchar(100) = N'note' + NCHAR(0xD800); RAISERROR(@m, 10, 1); SELECT 7",
    )
    .await?;
    assert_eq!(r[0].get::<i32, _>(0), Some(7));

    match conn
        .simple_query("DECLARE @m nvarchar(100) = N'boom' + NCHAR(0xDC00); RAISERROR(@m, 16, 1)")
        .await
    {
        Err(tiberius::error::Error::Server(e)) => assert_eq!(e.message(), "boom\u{FFFD}"),
        Err(e) => panic!("expected a server error, got {e:?}"),
        Ok(_) => panic!("RAISERROR must fail the query"),
    }

    let r = rows(&mut conn, "SELECT 42").await?;
    assert_eq!(r[0].get::<i32, _>(0), Some(42));

    Ok(())
}
