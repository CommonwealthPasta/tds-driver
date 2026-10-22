//! Value decoding against a live server.

use futures_util::io::{AsyncRead, AsyncWrite};
use once_cell::sync::Lazy;
use std::env;
use std::sync::Once;

use runtimes_macro::test_on_runtimes;
use tiberius::{numeric::Numeric, Result, Row};

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

#[test_on_runtimes]
async fn money_decodes_exactly<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let r = rows(
        &mut conn,
        "SELECT CAST(922337203685477.5807 AS money), CAST(922337203685477.5806 AS money), \
         CAST(-922337203685477.5808 AS money), CAST(214748.3647 AS smallmoney), \
         CAST(-214748.3648 AS smallmoney), CAST(1.23 AS money), CAST(2.33 AS smallmoney)",
    )
    .await?;
    let r = &r[0];

    let n = |i: usize| r.get::<Numeric, _>(i).unwrap().to_string();
    assert_eq!(n(0), "922337203685477.5807");
    assert_eq!(n(1), "922337203685477.5806");
    assert_eq!(n(2), "-922337203685477.5808");
    assert_eq!(n(3), "214748.3647");
    assert_eq!(n(4), "-214748.3648");

    // Existing f64 reads keep working.
    assert_eq!(r.get::<f64, _>(5), Some(1.23));
    assert_eq!(r.get::<f64, _>(6), Some(2.33));

    #[cfg(feature = "rust_decimal")]
    assert_eq!(
        r.get::<rust_decimal::Decimal, _>(0).unwrap().to_string(),
        "922337203685477.5807"
    );
    #[cfg(feature = "bigdecimal")]
    assert_eq!(
        r.get::<tiberius::numeric::BigDecimal, _>(2)
            .unwrap()
            .to_string(),
        "-922337203685477.5808"
    );

    Ok(())
}

#[test_on_runtimes]
async fn oem_code_page_collations<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let r = rows(
        &mut conn,
        "CREATE TABLE #o (a varchar(20) COLLATE SQL_Latin1_General_CP437_CI_AS, \
         b varchar(20) COLLATE SQL_Latin1_General_CP850_BIN, \
         c text COLLATE SQL_Scandinavian_CP850_CI_AS); \
         INSERT #o VALUES (N'Ç│π½', N'ÇØð¤ı', N'Åæø'); \
         SELECT a, b, c, CAST(a AS sql_variant) FROM #o",
    )
    .await?;

    assert_eq!(r[0].get::<&str, _>(0), Some("Ç│π½"));
    assert_eq!(r[0].get::<&str, _>(1), Some("ÇØð¤ı"));
    assert_eq!(r[0].get::<&str, _>(2), Some("Åæø"));
    assert_eq!(r[0].get::<&str, _>(3), Some("Ç│π½"));

    // Every byte decodes as the server converts it.
    let hex: String = (0x01u8..=0xff).map(|b| format!("{b:02X}")).collect();
    for coll in [
        "SQL_Latin1_General_CP437_BIN",
        "SQL_Latin1_General_CP850_BIN",
    ] {
        let sql = format!(
            "CREATE TABLE #b (v varchar(300) COLLATE {coll}); INSERT #b SELECT 0x{hex}; \
             SELECT v, CAST(v AS nvarchar(300)), CAST(v AS varbinary(300)) FROM #b; DROP TABLE #b"
        );
        let r = rows(&mut conn, &sql).await?;
        assert_eq!(
            r[0].get::<&[u8], _>(2).map(<[u8]>::len),
            Some(255),
            "{coll}"
        );
        assert_eq!(r[0].get::<&str, _>(0), r[0].get::<&str, _>(1), "{coll}");
    }

    Ok(())
}

#[test_on_runtimes]
async fn sql_variant_utf8_varchar<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let r = rows(
        &mut conn,
        "SELECT CAST(CAST(N'héllo 日€😀' COLLATE Latin1_General_100_CI_AS_SC_UTF8 AS varchar(50)) AS sql_variant), \
         CAST(N'héllo' COLLATE Latin1_General_100_CI_AS_SC_UTF8 AS varchar(50))",
    )
    .await?;

    assert_eq!(r[0].get::<&str, _>(0), Some("héllo 日€😀"));
    assert_eq!(r[0].get::<&str, _>(1), Some("héllo"));

    Ok(())
}

#[cfg(feature = "rust_decimal")]
#[test_on_runtimes]
async fn rust_decimal_out_of_range<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    use rust_decimal::Decimal;

    let r = rows(
        &mut conn,
        "SELECT CAST('12345678901234567890123456789012345678' AS decimal(38,0)), \
         CAST('0.12345678901234567890123456789012345678' AS decimal(38,38)), \
         CAST(0.5 AS decimal(38,30)), CAST('79228162514264337593543950335' AS decimal(38,0))",
    )
    .await?;
    let r = &r[0];

    assert!(r.try_get::<Decimal, _>(0).is_err());
    assert!(r.try_get::<Decimal, _>(1).is_err());
    // Trailing zeros beyond scale 28 are dropped, as SqlClient does.
    assert_eq!(r.try_get::<Decimal, _>(2)?, Some(Decimal::new(5, 1)));
    assert_eq!(r.try_get::<Decimal, _>(3)?, Some(Decimal::MAX));

    Ok(())
}

#[cfg(feature = "chrono")]
#[test_on_runtimes]
async fn chrono_utc_from_datetimeoffset<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    use chrono::{DateTime, FixedOffset, Utc};

    let r = rows(
        &mut conn,
        "SELECT CAST('2020-01-01 12:00:00.1234567 +02:00' AS datetimeoffset), \
         CAST('2020-01-01 00:30:00 -05:30' AS datetimeoffset(0))",
    )
    .await?;

    let utc: DateTime<Utc> = r[0].get(0).unwrap();
    let fixed: DateTime<FixedOffset> = r[0].get(0).unwrap();
    assert_eq!(utc.to_rfc3339(), "2020-01-01T10:00:00.123456700+00:00");
    assert_eq!(utc, fixed);

    let utc: DateTime<Utc> = r[0].get(1).unwrap();
    assert_eq!(utc.to_rfc3339(), "2020-01-01T06:00:00+00:00");

    Ok(())
}
