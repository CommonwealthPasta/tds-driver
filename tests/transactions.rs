//! Integration tests for the Transaction Manager requests (begin / commit /
//! rollback, and explicit isolation levels). These exercise the client-side
//! transaction API against a live SQL Server; they run in CI against the same
//! `mssql-2022` service the other integration tests use.

use futures_util::io::{AsyncRead, AsyncWrite};
use names::{Generator, Name};
use once_cell::sync::Lazy;
use std::cell::RefCell;
use std::env;
use std::sync::Once;

use runtimes_macro::test_on_runtimes;
use tiberius::{IsolationLevel, Result};

// This is used in the testing macro :)
#[allow(dead_code)]
static LOGGER_SETUP: Once = Once::new();

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TIBERIUS_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;user=SA;password=<YourStrong@Passw0rd>;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
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

async fn row_count<S>(conn: &mut tiberius::Client<S>, table: &str) -> Result<i32>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let row = conn
        .query(format!("SELECT COUNT(*) FROM ##{}", table), &[])
        .await?
        .into_row()
        .await?
        .expect("COUNT(*) always returns a row");

    Ok(row.get::<i32, _>(0).expect("COUNT(*) is never NULL"))
}

#[test_on_runtimes]
async fn transaction_commit_persists_rows<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = random_table().await;
    // Create the table outside the transaction so a rollback in a later test
    // cannot undo its existence.
    conn.execute(format!("CREATE TABLE ##{} (id int)", table), &[])
        .await?;

    conn.begin_transaction().await?;
    conn.execute(
        format!("INSERT INTO ##{} (id) VALUES (@P1), (@P2)", table),
        &[&1i32, &2i32],
    )
    .await?;
    conn.commit_transaction().await?;

    assert_eq!(2, row_count(&mut conn, &table).await?);

    Ok(())
}

#[test_on_runtimes]
async fn transaction_rollback_discards_rows<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = random_table().await;
    conn.execute(format!("CREATE TABLE ##{} (id int)", table), &[])
        .await?;
    // One row committed before the transaction.
    conn.execute(
        format!("INSERT INTO ##{} (id) VALUES (@P1)", table),
        &[&1i32],
    )
    .await?;

    conn.begin_transaction().await?;
    conn.execute(
        format!("INSERT INTO ##{} (id) VALUES (@P1), (@P2)", table),
        &[&2i32, &3i32],
    )
    .await?;
    conn.rollback_transaction().await?;

    // Only the pre-transaction row survives.
    assert_eq!(1, row_count(&mut conn, &table).await?);

    Ok(())
}

#[test_on_runtimes]
async fn transaction_with_explicit_isolation_levels<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = random_table().await;
    conn.execute(format!("CREATE TABLE ##{} (id int)", table), &[])
        .await?;

    // SNAPSHOT is intentionally omitted: it requires ALLOW_SNAPSHOT_ISOLATION to
    // be enabled on the database, which the default test database is not.
    for level in [
        IsolationLevel::ReadUncommitted,
        IsolationLevel::ReadCommitted,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
    ] {
        conn.begin_transaction_with_isolation(level).await?;
        conn.execute(
            format!("INSERT INTO ##{} (id) VALUES (@P1)", table),
            &[&1i32],
        )
        .await?;
        conn.commit_transaction().await?;
    }

    assert_eq!(4, row_count(&mut conn, &table).await?);

    Ok(())
}

#[test_on_runtimes]
async fn rolled_back_transaction_can_be_followed_by_a_new_one<S>(
    mut conn: tiberius::Client<S>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let table = random_table().await;
    conn.execute(format!("CREATE TABLE ##{} (id int)", table), &[])
        .await?;

    conn.begin_transaction().await?;
    conn.execute(
        format!("INSERT INTO ##{} (id) VALUES (@P1)", table),
        &[&10i32],
    )
    .await?;
    conn.rollback_transaction().await?;

    // The connection is reusable for a fresh transaction after a rollback.
    conn.begin_transaction().await?;
    conn.execute(
        format!("INSERT INTO ##{} (id) VALUES (@P1)", table),
        &[&20i32],
    )
    .await?;
    conn.commit_transaction().await?;

    assert_eq!(1, row_count(&mut conn, &table).await?);

    Ok(())
}

async fn session_isolation_level<S>(conn: &mut tiberius::Client<S>) -> Result<i16>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let row = conn
        .query(
            "SELECT transaction_isolation_level FROM sys.dm_exec_sessions \
             WHERE session_id = @@SPID",
            &[],
        )
        .await?
        .into_row()
        .await?
        .expect("the session row exists");

    Ok(row.get::<i16, _>(0).expect("never NULL"))
}

// sys.dm_exec_sessions.transaction_isolation_level values.
const READ_COMMITTED: i16 = 2;
const SERIALIZABLE: i16 = 4;

#[test_on_runtimes]
async fn begin_transaction_resets_isolation_to_read_committed<S>(
    mut conn: tiberius::Client<S>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    conn.begin_transaction_with_isolation(IsolationLevel::Serializable)
        .await?;
    conn.commit_transaction().await?;
    // The level outlives the transaction (MS-TDS 2.2.6.9).
    assert_eq!(SERIALIZABLE, session_isolation_level(&mut conn).await?);

    conn.begin_transaction().await?;
    let in_transaction = session_isolation_level(&mut conn).await?;
    conn.commit_transaction().await?;
    assert_eq!(READ_COMMITTED, in_transaction);
    assert_eq!(READ_COMMITTED, session_isolation_level(&mut conn).await?);

    Ok(())
}

#[test_on_runtimes]
async fn unspecified_isolation_keeps_session_level<S>(mut conn: tiberius::Client<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    conn.begin_transaction_with_isolation(IsolationLevel::Serializable)
        .await?;
    conn.commit_transaction().await?;

    conn.begin_transaction_with_isolation(IsolationLevel::Unspecified)
        .await?;
    let in_transaction = session_isolation_level(&mut conn).await?;
    conn.commit_transaction().await?;
    assert_eq!(SERIALIZABLE, in_transaction);

    Ok(())
}
