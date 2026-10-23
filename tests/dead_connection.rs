//! What a connection reports once the server has ended the session: the
//! cause, once, and from then on that the connection cannot be used.

use std::{env, io::ErrorKind, time::Duration};

use once_cell::sync::Lazy;
use tds_driver::{error::Error, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

static CONN_STR: Lazy<String> = Lazy::new(|| {
    env::var("TDS_DRIVER_TEST_CONNECTION_STRING").unwrap_or_else(|_| {
        "server=tcp:localhost,1433;IntegratedSecurity=true;TrustServerCertificate=true".to_owned()
    })
});

const IO_PREFIX: &str = "An error occurred during the attempt of performing I/O";

async fn connect() -> anyhow::Result<Client<Compat<TcpStream>>> {
    let config = Config::from_ado_string(&CONN_STR)?;
    let tcp = TcpStream::connect(config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Ok(Client::connect(config, tcp.compat_write()).await?)
}

async fn select_one(conn: &mut Client<Compat<TcpStream>>) -> tds_driver::Result<()> {
    conn.simple_query("SELECT 1").await?.into_results().await?;
    Ok(())
}

/// The first failure is the I/O error itself, wrapped once; every later use
/// says the connection is gone and why.
async fn assert_dead(conn: &mut Client<Compat<TcpStream>>) {
    let first = select_one(conn).await.unwrap_err();
    let Error::Io { kind, message } = &first else {
        panic!("expected an I/O error, got {first:?}");
    };
    assert_ne!(*kind, ErrorKind::NotConnected, "{first}");
    assert!(!message.contains(IO_PREFIX), "wrapped twice: {first}");
    assert_eq!(first.to_string().matches(IO_PREFIX).count(), 1, "{first}");
    let cause = message.clone();

    for _ in 0..2 {
        let later = select_one(conn).await.unwrap_err();
        let Error::Io { kind, message } = &later else {
            panic!("expected an I/O error, got {later:?}");
        };
        assert_eq!(*kind, ErrorKind::NotConnected, "{later}");
        assert!(message.contains(&cause), "{later} should name {cause}");
        assert!(!message.contains("cancel"), "{later}");
    }
}

#[tokio::test]
async fn a_killed_connection_reports_the_cause_once_then_not_connected() -> anyhow::Result<()> {
    let mut admin = connect().await?;
    let mut conn = connect().await?;

    let spid: i16 = conn
        .simple_query("SELECT @@SPID")
        .await?
        .into_row()
        .await?
        .unwrap()
        .get(0)
        .unwrap();
    admin
        .simple_query(format!("KILL {spid}"))
        .await?
        .into_results()
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_dead(&mut conn).await;

    Ok(())
}

#[tokio::test]
async fn a_severity_20_error_closes_the_connection() -> anyhow::Result<()> {
    let mut conn = connect().await?;

    let err = match conn
        .simple_query("RAISERROR('fatal', 20, 1) WITH LOG")
        .await
    {
        Ok(stream) => stream.into_results().await.unwrap_err(),
        Err(e) => e,
    };
    assert_eq!(err.code(), Some(50000), "{err}");

    assert_dead(&mut conn).await;

    Ok(())
}
