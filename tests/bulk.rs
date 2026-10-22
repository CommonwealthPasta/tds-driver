use futures_util::io::{AsyncRead, AsyncWrite};
use names::{Generator, Name};
use once_cell::sync::Lazy;
use std::cell::RefCell;
use std::env;
use std::future::Future;
use std::sync::Once;
use std::time::Duration;
use tiberius::ColumnData;
use tiberius::{IntoSql, Result, TokenRow};

#[cfg(all(feature = "tds73", feature = "chrono"))]
use chrono::DateTime;
#[cfg(all(feature = "tds73", feature = "chrono"))]
use chrono::NaiveDateTime;

use runtimes_macro::test_on_runtimes;

// This is used in the testing macro :)
#[allow(dead_code)]
static LOGGER_SETUP: Once = Once::new();

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TIBERIUS_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

thread_local! {
    static NAMES: RefCell<Option<Generator<'static>>> =
        const { RefCell::new(None) };
}

async fn random_table() -> String {
    NAMES.with(|maybe_generator| {
        maybe_generator
            .borrow_mut()
            .get_or_insert_with(|| Generator::with_naming(Name::Plain))
            .next()
            .unwrap()
            .replace('-', "")
    })
}

macro_rules! test_bulk_type {
    ($name:ident($sql_type:literal, $total_generated:expr, $generator:expr)) => {
        paste::item! {
            #[test_on_runtimes]
            async fn [< bulk_load_optional_ $name >]<S>(mut conn: tiberius::Client<S>) -> Result<()>
            where
                S: AsyncRead + AsyncWrite + Unpin + Send,
            {
                let table = format!("##{}", random_table().await);

                conn.execute(
                    &format!(
                        "CREATE TABLE {} (id INT IDENTITY PRIMARY KEY, content {} NULL)",
                        table,
                        $sql_type,
                    ),
                    &[],
                )
                    .await?;

                let mut req = conn.bulk_insert(&table).await?;

                for i in $generator {
                    let mut row = TokenRow::new();
                    row.push(i.into_sql());
                    req.send(row).await?;
                }

                let res = req.finalize().await?;

                assert_eq!($total_generated, res.total());

                Ok(())
            }

            #[test_on_runtimes]
            async fn [< bulk_load_required_ $name >]<S>(mut conn: tiberius::Client<S>) -> Result<()>
            where
                S: AsyncRead + AsyncWrite + Unpin + Send,
            {
                let table = format!("##{}", random_table().await);

                conn.execute(
                    &format!(
                        "CREATE TABLE {} (id INT IDENTITY PRIMARY KEY, content {} NOT NULL)",
                        table,
                        $sql_type
                    ),
                    &[],
                )
                    .await?;

                let mut req = conn.bulk_insert(&table).await?;

                for i in $generator {
                    let mut row = TokenRow::new();
                    row.push(i.into_sql());
                    req.send(row).await?;
                }

                let res = req.finalize().await?;

                assert_eq!($total_generated, res.total());

                Ok(())
            }
        }
    };
}

test_bulk_type!(tinyint("TINYINT", 256, 0..=255u8));
test_bulk_type!(smallint("SMALLINT", 2000, 0..2000i16));
test_bulk_type!(int("INT", 2000, 0..2000i32));
test_bulk_type!(bigint("BIGINT", 2000, 0..2000i64));

test_bulk_type!(empty_varchar(
    "VARCHAR(MAX)",
    100,
    vec![""; 100].into_iter()
));
test_bulk_type!(empty_nvarchar(
    "NVARCHAR(MAX)",
    100,
    vec![""; 100].into_iter()
));
test_bulk_type!(empty_varbinary(
    "VARBINARY(MAX)",
    100,
    vec![b""; 100].into_iter()
));

test_bulk_type!(real(
    "REAL",
    1000,
    vec![std::f32::consts::PI; 1000].into_iter()
));

test_bulk_type!(float(
    "FLOAT",
    1000,
    vec![std::f64::consts::PI; 1000].into_iter()
));

test_bulk_type!(varchar_limited(
    "VARCHAR(255)",
    1000,
    vec!["aaaaaaaaaaaaaaaaaaaaaaa"; 1000].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2(
    "DATETIME2",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_naive("DATETIME2", 100, {
    #[allow(deprecated)]
    let dt = NaiveDateTime::from_timestamp_opt(1658524194, 123456789).unwrap();

    vec![dt; 100].into_iter()
}));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_0(
    "DATETIME2(0)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_1(
    "DATETIME2(1)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_2(
    "DATETIME2(2)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_3(
    "DATETIME2(3)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_4(
    "DATETIME2(4)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_5(
    "DATETIME2(5)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_6(
    "DATETIME2(6)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[cfg(all(feature = "tds73", feature = "chrono"))]
test_bulk_type!(datetime2_7(
    "DATETIME2(7)",
    100,
    vec![DateTime::from_timestamp(1658524194, 123456789); 100].into_iter()
));

#[test_on_runtimes]
async fn read_and_write_to_keyword_columns<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = format!("##{}", random_table().await);

    conn.simple_query(format!("CREATE TABLE {} ([End] INT)", table))
        .await?;

    let mut req = conn.bulk_insert(&table).await.unwrap();
    for num in [6, 7, 8] {
        let mut row = TokenRow::new();
        row.push(ColumnData::I32(Some(num)));
        req.send(row).await.unwrap();
    }
    let result = req.finalize().await.unwrap();
    assert_eq!(result.rows_affected(), &[3]);

    let rows = conn
        .query(format!("SELECT [End] FROM {}", table), &[])
        .await?
        .into_first_result()
        .await?;

    assert_eq!(rows.len(), 3);
    assert_eq!(Some(6), rows[0].get(0));
    assert_eq!(Some(7), rows[1].get(0));
    assert_eq!(Some(8), rows[2].get(0));

    Ok(())
}

macro_rules! test_bulk_columns {
    ($name:ident($total_generated:literal $(, $sql_type:literal)+ $(, ($cols:expr, $generator:expr ))+ $(,)?)) => {
        paste::item! {
            #[test_on_runtimes]
            async fn [< bulk_load_optional_ $name >]<S>(mut conn: tiberius::Client<S>) -> Result<()>
            where
                S: AsyncRead + AsyncWrite + Unpin + Send,
            {
                use tiberius::IntoRow;

                let table = format!("##{}", random_table().await);
                let column_defs = &[$($sql_type,)+];

                conn.execute(
                    &format!(
                        "CREATE TABLE {} (id INT IDENTITY PRIMARY KEY, {})",
                        table,
                        column_defs.join(", "),
                    ),
                    &[],
                )
                    .await?;

                let mut count = 0;

                $(
                    let mut req = conn.bulk_insert_columns(&table, $cols).await?;
                    for i in $generator {
                        let row = i.into_row();
                        req.send(row).await?;
                    }

                    let res = req.finalize().await?;
                    count += res.total();
                )+
                assert_eq!($total_generated, count);

                Ok(())
            }

            #[test_on_runtimes]
            async fn [< bulk_load_required_ $name >]<S>(mut conn: tiberius::Client<S>) -> Result<()>
            where
                S: AsyncRead + AsyncWrite + Unpin + Send,
            {
                use tiberius::IntoRow;
                let table = format!("##{}", random_table().await);
                let column_defs = &[$(format!("{} NOT NULL", $sql_type),)+];

                conn.execute(
                    &format!(
                        "CREATE TABLE {} (id INT IDENTITY PRIMARY KEY, {})",
                        table,
                        column_defs.join(", "),
                    ),
                    &[],
                )
                    .await?;

                let mut count = 0;

                $(
                    let mut req = conn.bulk_insert_columns(&table, $cols).await?;
                    for i in $generator {
                        let row = i.into_row();
                        req.send(row).await?;
                    }

                    let res = req.finalize().await?;
                    count += res.total();
                )+
                assert_eq!($total_generated, count);

                Ok(())
            }

        }
    };
}

test_bulk_columns!(ab_ba_default_columns(
    200,
    "a INT",
    "b FLOAT",
    "c INT DEFAULT 0",
    (&["a", "b"], vec![(1i32, 1f64); 100]),
    (&["b", "a"], vec![(2f64, 2i32); 100]),
));

test_bulk_columns!(ab_ba_override_default_columns(
    200,
    "a INT",
    "b FLOAT",
    "c INT DEFAULT 0",
    (&["a", "b", "c"], vec![(1i32, 1f64, 10i32); 100]),
    (&["b", "c", "a"], vec![(2f64, 20i32, 2i32); 100]),
));

/// Fails rather than hangs when the connection is stuck.
async fn within<T>(fut: impl Future<Output = Result<T>>) -> Result<T> {
    use futures_util::future::{select, Either};
    match select(Box::pin(fut), smol::Timer::after(Duration::from_secs(15))).await {
        Either::Left((res, _)) => res,
        Either::Right(_) => panic!("no response within 15s: the connection is stuck"),
    }
}

async fn select_int<S>(conn: &mut tiberius::Client<S>, sql: &str) -> Result<Option<i32>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    within(async {
        let row = conn.simple_query(sql).await?.into_row().await?;
        Ok(row.and_then(|r| r.get::<i32, _>(0)))
    })
    .await
}

#[test_on_runtimes]
async fn bulk_load_dropped_mid_stream_cancels_it<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    use tiberius::IntoRow;
    let table = format!("##{}", random_table().await);
    conn.simple_query(format!("CREATE TABLE {table} (id INT, s NVARCHAR(400))"))
        .await?
        .into_results()
        .await?;

    {
        let mut req = conn.bulk_insert(&table).await?;
        let s = "x".repeat(400);
        // 16 KB: several packets are on the wire, the message is not ended.
        for i in 0..20i32 {
            req.send((i, s.as_str()).into_row()).await?;
        }
    }

    assert_eq!(Some(42), select_int(&mut conn, "SELECT 42").await?);
    let count = format!("SELECT COUNT(*) FROM {table}");
    assert_eq!(Some(0), select_int(&mut conn, &count).await?);

    let mut req = conn.bulk_insert(&table).await?;
    req.send((1i32, "a").into_row()).await?;
    assert_eq!(1, req.finalize().await?.total());

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_dropped_before_any_packet<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = format!("##{}", random_table().await);
    conn.simple_query(format!("CREATE TABLE {table} (id INT)"))
        .await?
        .into_results()
        .await?;

    drop(conn.bulk_insert(&table).await?);

    assert_eq!(Some(42), select_int(&mut conn, "SELECT 42").await?);

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_cancel_query_after_drop<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    use tiberius::IntoRow;
    let table = format!("##{}", random_table().await);
    conn.simple_query(format!("CREATE TABLE {table} (id INT)"))
        .await?
        .into_results()
        .await?;

    {
        let mut req = conn.bulk_insert(&table).await?;
        req.send(1i32.into_row()).await?;
    }
    within(conn.cancel_query()).await?;

    assert_eq!(Some(42), select_int(&mut conn, "SELECT 42").await?);

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_row_error_keeps_request_usable<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = format!("##{}", random_table().await);
    conn.simple_query(format!("CREATE TABLE {table} (a INT, s VARCHAR(10))"))
        .await?
        .into_results()
        .await?;

    let mut req = conn.bulk_insert(&table).await?;

    let mut too_long = TokenRow::new();
    too_long.push(1i32.into_sql());
    too_long.push("longer than ten".into_sql());
    assert!(req.send(too_long).await.is_err());

    let mut too_few = TokenRow::new();
    too_few.push(2i32.into_sql());
    assert!(req.send(too_few).await.is_err());

    let mut good = TokenRow::new();
    good.push(3i32.into_sql());
    good.push("ok".into_sql());
    req.send(good).await?;
    assert_eq!(1, within(req.finalize()).await?.total());

    let sum = format!("SELECT SUM(a) FROM {table}");
    assert_eq!(Some(3), select_int(&mut conn, &sum).await?);

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_row_error_then_drop<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = format!("##{}", random_table().await);
    conn.simple_query(format!("CREATE TABLE {table} (s VARCHAR(10))"))
        .await?
        .into_results()
        .await?;

    {
        let mut req = conn.bulk_insert(&table).await?;
        let mut row = TokenRow::new();
        row.push("longer than ten".into_sql());
        assert!(req.send(row).await.is_err());
    }

    assert_eq!(Some(42), select_int(&mut conn, "SELECT 42").await?);

    Ok(())
}

/// Bulk inserts `value` into a one-column table `v {column}` and reads
/// `readback`, an expression over `v`, back as text.
async fn bulk_one<S>(
    conn: &mut tiberius::Client<S>,
    column: &str,
    value: ColumnData<'static>,
    readback: &str,
) -> Result<Option<String>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = format!("##{}", random_table().await);
    conn.simple_query(format!("CREATE TABLE {table} (v {column})"))
        .await?
        .into_results()
        .await?;

    let mut req = conn.bulk_insert(&table).await?;
    let mut row = TokenRow::new();
    row.push(value);
    req.send(row).await?;
    within(req.finalize()).await?;

    let row = conn
        .simple_query(format!(
            "SELECT CONVERT(nvarchar(200), {readback}) FROM {table}"
        ))
        .await?
        .into_row()
        .await?;

    Ok(row.and_then(|r| r.get::<&str, _>(0).map(str::to_owned)))
}

async fn assert_bulk<S>(
    conn: &mut tiberius::Client<S>,
    cases: Vec<(&str, ColumnData<'static>, &str, &str)>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    for (column, value, readback, expected) in cases {
        let got = bulk_one(conn, column, value.clone(), readback).await?;
        assert_eq!(Some(expected), got.as_deref(), "{column} <- {value:?}");
    }

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_converts_numbers_to_the_column_width<S>(
    mut conn: tiberius::Client<S>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    assert_bulk(
        &mut conn,
        vec![
            ("BIGINT", (-5i32).into_sql(), "v", "-5"),
            ("BIGINT", i32::MIN.into_sql(), "v", "-2147483648"),
            ("INT", (-1i16).into_sql(), "v", "-1"),
            ("BIGINT NOT NULL", (-5i32).into_sql(), "v", "-5"),
            ("INT", 5i64.into_sql(), "v", "5"),
            ("INT NOT NULL", 5i64.into_sql(), "v", "5"),
            ("TINYINT", 7i32.into_sql(), "v", "7"),
            ("FLOAT", (-2.5f32).into_sql(), "v", "-2.5"),
            ("FLOAT NOT NULL", 0.5f32.into_sql(), "v", "0.5"),
            ("REAL", 1.5f64.into_sql(), "v", "1.5"),
        ],
    )
    .await?;

    let err = bulk_one(&mut conn, "INT", 5_000_000_000i64.into_sql(), "v")
        .await
        .unwrap_err();
    assert!(
        matches!(err, tiberius::error::Error::Conversion(_)),
        "{err:?}"
    );

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_encodes_varchar_in_the_column_collation<S>(
    mut conn: tiberius::Client<S>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // A local temp table: the collation lookup has to find it too.
    let table = format!("#{}", random_table().await);
    conn.simple_query(format!(
        "CREATE TABLE {table} (g VARCHAR(10) COLLATE Greek_CI_AS, j CHAR(4) COLLATE Japanese_CI_AS, \
         t TEXT COLLATE Japanese_CI_AS, n NVARCHAR(10) COLLATE Greek_CI_AS, d VARCHAR(10))"
    ))
    .await?
    .into_results()
    .await?;

    let mut req = conn.bulk_insert(&table).await?;
    let mut row = TokenRow::new();
    for s in [
        "\u{3b1}\u{3b2}",
        "\u{65e5}\u{672c}",
        "\u{65e5}\u{672c}",
        "\u{3c9}",
        "\u{e9}",
    ] {
        row.push(s.into_sql());
    }
    req.send(row).await?;

    let mut unencodable = TokenRow::new();
    for s in ["\u{65e5}", "", "", "", ""] {
        unencodable.push(s.into_sql());
    }
    let err = req.send(unencodable).await.unwrap_err();
    assert!(
        matches!(err, tiberius::error::Error::Encoding(_)),
        "{err:?}"
    );

    assert_eq!(1, within(req.finalize()).await?.total());

    let row = conn
        .simple_query(format!(
            "SELECT CAST(g AS nvarchar(10)), CAST(j AS nvarchar(10)), \
             CAST(CAST(t AS varchar(10)) AS nvarchar(10)), n, CAST(d AS nvarchar(10)) FROM {table}"
        ))
        .await?
        .into_row()
        .await?
        .unwrap();

    let got: Vec<_> = (0..5).map(|i| row.get::<&str, _>(i).unwrap()).collect();
    assert_eq!(
        got,
        [
            "\u{3b1}\u{3b2}",
            "\u{65e5}\u{672c}",
            "\u{65e5}\u{672c}",
            "\u{3c9}",
            "\u{e9}"
        ]
    );

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_max_values_over_64k<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    assert_bulk(
        &mut conn,
        vec![
            (
                "VARCHAR(MAX)",
                "a".repeat(70_000).into_sql(),
                "DATALENGTH(v)",
                "70000",
            ),
            (
                "NVARCHAR(MAX)",
                "a".repeat(40_000).into_sql(),
                "DATALENGTH(v)",
                "80000",
            ),
            (
                "VARBINARY(MAX) NOT NULL",
                vec![7u8; 70_000].into_sql(),
                "DATALENGTH(v)",
                "70000",
            ),
        ],
    )
    .await
}

#[test_on_runtimes]
async fn bulk_load_nvarchar_up_to_4000<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    assert_bulk(
        &mut conn,
        vec![
            (
                "NVARCHAR(3000)",
                "a".repeat(3000).into_sql(),
                "LEN(v)",
                "3000",
            ),
            (
                "NVARCHAR(4000) NOT NULL",
                "\u{e9}".repeat(4000).into_sql(),
                "LEN(v)",
                "4000",
            ),
            ("NCHAR(2001)", "ab".into_sql(), "DATALENGTH(v)", "4002"),
        ],
    )
    .await
}

#[test_on_runtimes]
async fn bulk_load_text_ntext_image<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    assert_bulk(
        &mut conn,
        vec![
            (
                "TEXT",
                "hello".into_sql(),
                "CAST(v AS varchar(10))",
                "hello",
            ),
            (
                "NTEXT NOT NULL",
                "h\u{e9}llo".into_sql(),
                "CAST(v AS nvarchar(10))",
                "h\u{e9}llo",
            ),
            (
                "NTEXT",
                "x".repeat(10_000).into_sql(),
                "DATALENGTH(v)",
                "20000",
            ),
            ("IMAGE", vec![1u8, 2, 3].into_sql(), "DATALENGTH(v)", "3"),
            (
                "TEXT",
                None::<&str>.into_sql(),
                "ISNULL(CAST(v AS varchar(10)), 'NULL')",
                "NULL",
            ),
            (
                "IMAGE",
                None::<Vec<u8>>.into_sql(),
                "ISNULL(CAST(DATALENGTH(v) AS varchar(10)), 'NULL')",
                "NULL",
            ),
        ],
    )
    .await
}

#[test_on_runtimes]
async fn bulk_load_sql_variant_string<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let collation = conn
        .simple_query("SELECT CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS nvarchar(128))")
        .await?
        .into_row()
        .await?
        .and_then(|r| r.get::<&str, _>(0).map(str::to_owned));

    assert_bulk(
        &mut conn,
        vec![
            ("SQL_VARIANT", "abc".into_sql(), "v", "abc"),
            (
                "SQL_VARIANT",
                "abc".into_sql(),
                "SQL_VARIANT_PROPERTY(v, 'Collation')",
                collation.as_deref().unwrap(),
            ),
            (
                "SQL_VARIANT",
                "abc".into_sql(),
                "SQL_VARIANT_PROPERTY(v, 'BaseType')",
                "nvarchar",
            ),
        ],
    )
    .await
}

struct VariantRow {
    v: &'static str,
    b: i32,
}

impl<'a> tiberius::TableValueRow<'a> for VariantRow {
    fn bind_fields(&self, row: &mut tiberius::SqlTableDataRow<'a>) {
        row.add_field(self.v);
        row.add_field(self.b);
    }

    fn get_db_type() -> &'static str {
        "VariantRow"
    }
}

#[test_on_runtimes]
async fn tvp_sql_variant_string_and_wider_column<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let db_type = random_table().await;
    let proc_name = random_table().await;
    conn.simple_query(format!(
        "CREATE TYPE dbo.[{db_type}] AS TABLE (v SQL_VARIANT, b BIGINT)"
    ))
    .await?
    .into_results()
    .await?;
    conn.simple_query(format!(
        "CREATE PROCEDURE dbo.[{proc_name}] @t dbo.[{db_type}] READONLY AS \
         SELECT CONVERT(nvarchar(10), v), b FROM @t"
    ))
    .await?
    .into_results()
    .await?;

    let result = async {
        let mut cmd = tiberius::Command::new(format!("dbo.[{proc_name}]"));
        cmd.bind_table_with_dbtype("@t", &db_type, vec![VariantRow { v: "abc", b: -5 }]);
        let result =
            within(async { cmd.exec(&mut conn).await?.into_command_result().await }).await?;
        let rows = result.to_query_result(0).unwrap();
        Ok::<_, tiberius::error::Error>((
            rows[0].get::<&str, _>(0).map(str::to_owned),
            rows[0].get::<i64, _>(1),
        ))
    }
    .await;

    within(async {
        conn.simple_query(format!(
            "DROP PROCEDURE dbo.[{proc_name}]; DROP TYPE dbo.[{db_type}]"
        ))
        .await?
        .into_results()
        .await
    })
    .await?;

    assert_eq!((Some("abc".to_owned()), Some(-5)), result?);

    Ok(())
}

#[test_on_runtimes]
async fn bulk_load_not_null_money<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    use tiberius::numeric::Numeric;

    assert_bulk(
        &mut conn,
        vec![
            (
                "MONEY NOT NULL",
                1.5f64.into_sql(),
                "CONVERT(varchar(30), v, 2)",
                "1.5000",
            ),
            (
                "MONEY NOT NULL",
                Numeric::new_with_scale(-15, 1).into_sql(),
                "CONVERT(varchar(30), v, 2)",
                "-1.5000",
            ),
            (
                "SMALLMONEY NOT NULL",
                (-1.25f64).into_sql(),
                "CONVERT(varchar(30), v, 2)",
                "-1.2500",
            ),
        ],
    )
    .await
}

#[test_on_runtimes]
async fn bulk_load_escapes_column_names<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = format!("##{}", random_table().await);
    conn.simple_query(format!("CREATE TABLE {table} ([we]]ird] INT, [x y] INT)"))
        .await?
        .into_results()
        .await?;

    let mut req = conn.bulk_insert(&table).await?;
    let mut row = TokenRow::new();
    row.push(1i32.into_sql());
    row.push(2i32.into_sql());
    req.send(row).await?;
    assert_eq!(1, within(req.finalize()).await?.total());

    let sum = format!("SELECT [we]]ird] * 10 + [x y] FROM {table}");
    assert_eq!(Some(12), select_int(&mut conn, &sum).await?);

    Ok(())
}

#[cfg(all(feature = "tds73", feature = "chrono"))]
fn ndt(h: u32, m: u32, s: u32, nanos: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 5, 6)
        .unwrap()
        .and_hms_nano_opt(h, m, s, nanos)
        .unwrap()
}

#[cfg(all(feature = "tds73", feature = "chrono"))]
#[test_on_runtimes]
async fn bulk_load_time_and_datetimeoffset_scale<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    use chrono::{FixedOffset, NaiveTime, TimeZone};

    let t = NaiveTime::from_hms_nano_opt(13, 14, 15, 123_456_700).unwrap();
    let dto = FixedOffset::east_opt(2 * 3600)
        .unwrap()
        .from_local_datetime(&ndt(13, 14, 15, 123_456_700))
        .unwrap();
    let rb = "CONVERT(varchar(40), v, 121)";

    assert_bulk(
        &mut conn,
        vec![
            ("TIME(3)", t.into_sql(), rb, "13:14:15.123"),
            ("TIME(0) NOT NULL", t.into_sql(), rb, "13:14:15"),
            (
                "DATETIMEOFFSET(3)",
                dto.into_sql(),
                rb,
                "2024-05-06 13:14:15.123 +02:00",
            ),
            (
                "DATETIMEOFFSET(0)",
                dto.into_sql(),
                rb,
                "2024-05-06 13:14:15 +02:00",
            ),
            (
                "DATETIMEOFFSET(3)",
                None::<DateTime<FixedOffset>>.into_sql(),
                "ISNULL(CONVERT(varchar(40), v, 121), 'NULL')",
                "NULL",
            ),
        ],
    )
    .await
}

#[cfg(all(feature = "tds73", feature = "chrono"))]
#[test_on_runtimes]
async fn bulk_load_datetime_from_datetime2<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let rb = "CONVERT(varchar(40), v, 121)";
    let early = chrono::NaiveDate::from_ymd_opt(1800, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();

    assert_bulk(
        &mut conn,
        vec![
            // Rounded to 1/300 s as CAST(datetime2 AS datetime) rounds.
            (
                "DATETIME",
                ndt(23, 59, 59, 999_000_000).into_sql(),
                rb,
                "2024-05-07 00:00:00.000",
            ),
            (
                "DATETIME",
                ndt(13, 14, 15, 1_900_000).into_sql(),
                rb,
                "2024-05-06 13:14:15.003",
            ),
            ("DATETIME", early.into_sql(), rb, "1800-01-01 00:00:00.000"),
            (
                "DATETIME NOT NULL",
                ndt(13, 14, 15, 123_456_700).into_sql(),
                rb,
                "2024-05-06 13:14:15.123",
            ),
            (
                "SMALLDATETIME NOT NULL",
                ndt(13, 14, 31, 0).into_sql(),
                rb,
                "2024-05-06 13:15:00.000",
            ),
            (
                "SMALLDATETIME",
                ndt(13, 14, 29, 999_000_000).into_sql(),
                rb,
                "2024-05-06 13:14:00.000",
            ),
        ],
    )
    .await
}
