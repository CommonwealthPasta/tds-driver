//! Value decoding against a live server.

use futures_util::io::{AsyncRead, AsyncWrite};
use once_cell::sync::Lazy;
use std::env;
use std::sync::Once;

use runtimes_macro::test_on_runtimes;
use tds_driver::{numeric::Numeric, ColumnData, ColumnType, Result, Row};

// This is used in the testing macro :)
#[allow(dead_code)]
static LOGGER_SETUP: Once = Once::new();

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TDS_DRIVER_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;user=SA;password=<YourStrong@Passw0rd>;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

async fn rows<S>(conn: &mut tds_driver::Client<S>, sql: &str) -> Result<Vec<Row>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    conn.simple_query(sql).await?.into_first_result().await
}

#[test_on_runtimes]
async fn lone_utf16_surrogates_decode_lossily<S>(mut conn: tds_driver::Client<S>) -> Result<()>
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
        Err(tds_driver::error::Error::Server(e)) => assert_eq!(e.message(), "boom\u{FFFD}"),
        Err(e) => panic!("expected a server error, got {e:?}"),
        Ok(_) => panic!("RAISERROR must fail the query"),
    }

    let r = rows(&mut conn, "SELECT 42").await?;
    assert_eq!(r[0].get::<i32, _>(0), Some(42));

    Ok(())
}

#[test_on_runtimes]
async fn money_decodes_exactly<S>(mut conn: tds_driver::Client<S>) -> Result<()>
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
        r.get::<tds_driver::numeric::BigDecimal, _>(2)
            .unwrap()
            .to_string(),
        "-922337203685477.5808"
    );

    Ok(())
}

/// A wide all-NULL row makes the server send NBCROW; a one-column SELECT gets ROW.
#[test_on_runtimes]
async fn nullable_money_and_smalldatetime<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let pad: Vec<String> = (0..20).map(|i| format!("p{i} int NULL")).collect();
    conn.simple_query(format!(
        "CREATE TABLE #n (sm smallmoney NULL, m money NULL, sdt smalldatetime NULL, \
         dt datetime NULL, {}); INSERT #n DEFAULT VALUES",
        pad.join(", ")
    ))
    .await?
    .into_results()
    .await?;

    let expected = [
        ("sm", ColumnType::Money4, ColumnData::Numeric(None)),
        ("m", ColumnType::Money, ColumnData::Numeric(None)),
        (
            "sdt",
            ColumnType::Datetime4,
            ColumnData::SmallDateTime(None),
        ),
        ("dt", ColumnType::Datetimen, ColumnData::DateTime(None)),
    ];

    let wide = rows(&mut conn, "SELECT * FROM #n").await?;
    for (i, (name, ty, null)) in expected.iter().enumerate() {
        assert_eq!(wide[0].columns()[i].column_type(), *ty, "{name}");
        assert_eq!(wide[0].get_column_data(i)?, null, "{name} (NBCROW)");

        let narrow = rows(&mut conn, &format!("SELECT {name} FROM #n")).await?;
        assert_eq!(narrow[0].get_column_data(0)?, null, "{name} (ROW)");
    }

    assert_eq!(wide[0].get::<f64, _>(0), None);

    Ok(())
}

#[test_on_runtimes]
async fn oem_code_page_collations<S>(mut conn: tds_driver::Client<S>) -> Result<()>
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
async fn sql_variant_utf8_varchar<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // UTF-8 collations arrived in SQL Server 2019.
    let utf8 = rows(
        &mut conn,
        "SELECT COUNT(*) FROM sys.fn_helpcollations() WHERE name = 'Latin1_General_100_CI_AS_SC_UTF8'",
    )
    .await?;
    if utf8[0].get::<i32, _>(0) == Some(0) {
        return Ok(());
    }

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

/// 0x82 0x20, a Shift-JIS lead byte with an invalid trail byte, is stored as
/// it is; it decodes as U+FFFD, then the space.
#[test_on_runtimes]
async fn invalid_code_page_sequences_decode_lossily<S>(
    mut conn: tds_driver::Client<S>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let r = rows(
        &mut conn,
        "CREATE TABLE #j (v varchar(10) COLLATE Japanese_CI_AS, c char(4) COLLATE Japanese_CI_AS, \
         t text COLLATE Japanese_CI_AS, m varchar(max) COLLATE Japanese_CI_AS, s sql_variant); \
         INSERT #j (v, c) VALUES (CAST(0x418220 AS varbinary(10)), CAST(0x418220 AS varbinary(10))); \
         UPDATE #j SET t = v, m = v, s = v; \
         SELECT v, c, t, m, s, CAST(v AS varbinary(10)) FROM #j",
    )
    .await?;

    assert_eq!(r[0].get::<&[u8], _>(5), Some(&[0x41, 0x82, 0x20][..]));
    for i in [0, 2, 3, 4] {
        assert_eq!(r[0].get::<&str, _>(i), Some("A\u{FFFD} "), "column {i}");
    }
    assert_eq!(r[0].get::<&str, _>(1), Some("A\u{FFFD}  "));

    let r = rows(&mut conn, "SELECT 42").await?;
    assert_eq!(r[0].get::<i32, _>(0), Some(42));

    Ok(())
}

#[test_on_runtimes]
async fn numeric_precision_and_display<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let r = rows(
        &mut conn,
        "SELECT CAST(0.5 AS decimal(38,38)), \
         CAST('-0.12345678901234567890123456789012345678' AS decimal(38,38)), \
         CAST(123 AS decimal(10,0)), CAST(-0.5 AS decimal(5,1))",
    )
    .await?;

    let a: Numeric = r[0].get(0).unwrap();
    let b: Numeric = r[0].get(1).unwrap();
    assert_eq!(a.precision(), 38);
    assert_eq!(b.precision(), 38);

    // A decimal(38,38) read from the server can be sent back.
    let back = conn
        .query("SELECT @P1, @P2", &[&a, &b])
        .await?
        .into_row()
        .await?
        .unwrap();
    assert_eq!(back.get::<Numeric, _>(0), Some(a));
    assert_eq!(back.get::<Numeric, _>(1), Some(b));

    assert_eq!(r[0].get::<Numeric, _>(2).unwrap().to_string(), "123");
    assert_eq!(r[0].get::<Numeric, _>(3).unwrap().to_string(), "-0.5");

    Ok(())
}

#[cfg(feature = "rust_decimal")]
#[test_on_runtimes]
async fn rust_decimal_out_of_range<S>(mut conn: tds_driver::Client<S>) -> Result<()>
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

#[cfg(all(feature = "chrono", feature = "tds73"))]
#[test_on_runtimes]
async fn chrono_utc_from_datetimeoffset<S>(mut conn: tds_driver::Client<S>) -> Result<()>
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

#[cfg(any(feature = "chrono", feature = "time"))]
const DATETIMES: &str = "SELECT v, CONVERT(varchar(30), v, 121) FROM (VALUES \
    (CAST('2023-01-01 12:34:56.997' AS datetime)), (CAST('2023-01-01 12:34:56.003' AS datetime)), \
    (CAST('2023-01-01 12:34:56.007' AS datetime)), (CAST('9999-12-31 23:59:59.997' AS datetime)), \
    (CAST('1753-01-01 00:00:00.000' AS datetime))) t(v)";

#[cfg(feature = "chrono")]
#[test_on_runtimes]
async fn chrono_datetime_rounds_to_milliseconds<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    for r in rows(&mut conn, DATETIMES).await? {
        let d: chrono::NaiveDateTime = r.get(0).unwrap();
        let server: &str = r.get(1).unwrap();
        assert_eq!(
            d.format("%Y-%m-%d %H:%M:%S%.9f").to_string(),
            format!("{server}000000")
        );
    }

    Ok(())
}

#[cfg(feature = "time")]
#[test_on_runtimes]
async fn time_datetime_rounds_to_milliseconds<S>(mut conn: tds_driver::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    for r in rows(&mut conn, DATETIMES).await? {
        let d: time::PrimitiveDateTime = r.get(0).unwrap();
        let server: &str = r.get(1).unwrap();
        let mine = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09}",
            d.year(),
            d.month() as u8,
            d.day(),
            d.hour(),
            d.minute(),
            d.second(),
            d.nanosecond()
        );
        assert_eq!(mine, format!("{server}000000"));
    }

    Ok(())
}
