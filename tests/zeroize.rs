//! No heap block freed while parsing a connection string or logging in may
//! still hold a credential: plaintext, UTF-16 or LOGIN7-obfuscated. A
//! scanning global allocator checks every block as it is freed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::compat::TokioAsyncWriteCompatExt;

struct Scan;

static ARMED: AtomicBool = AtomicBool::new(false);
static MARKERS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
static HITS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Scan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Disarmed while scanning so the lock and the scan don't recurse.
        if ARMED.swap(false, Ordering::SeqCst) {
            let block = std::slice::from_raw_parts(ptr, layout.size());
            if let Ok(markers) = MARKERS.try_lock() {
                if markers
                    .iter()
                    .any(|m| block.windows(m.len()).any(|w| w == m.as_slice()))
                {
                    HITS.fetch_add(1, Ordering::SeqCst);
                }
            }
            ARMED.store(true, Ordering::SeqCst);
        }
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static GLOBAL: Scan = Scan;

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn obfuscated(s: &str) -> Vec<u8> {
    utf16(s)
        .into_iter()
        .map(|b| b.rotate_left(4) ^ 0xA5)
        .collect()
}

/// Counts freed blocks holding any form of `secret` while `f` runs.
fn leaks<T>(secret: &str, f: impl FnOnce() -> T) -> (usize, T) {
    *MARKERS.lock().unwrap() = vec![
        secret.as_bytes().to_vec(),
        utf16(secret),
        obfuscated(secret),
    ];
    HITS.store(0, Ordering::SeqCst);
    ARMED.store(true, Ordering::SeqCst);
    let out = f();
    ARMED.store(false, Ordering::SeqCst);
    (HITS.load(Ordering::SeqCst), out)
}

/// Accepts one connection, answers PRELOGIN with ENCRYPT_NOT_SUP, reads the
/// LOGIN7 and closes. Its own buffers are wiped before they are freed.
async fn fake_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        for round in 0..2 {
            loop {
                let mut h = [0u8; 8];
                if s.read_exact(&mut h).await.is_err() {
                    return;
                }
                let mut body = vec![0u8; u16::from_be_bytes([h[2], h[3]]) as usize - 8];
                s.read_exact(&mut body).await.unwrap();
                body.iter_mut().for_each(|b| *b = 0);
                if h[1] & 1 == 1 {
                    break;
                }
            }
            if round == 0 {
                let mut p = vec![4u8, 1, 0, 26, 0, 0, 1, 0];
                p.extend_from_slice(&[0x00, 0, 11, 0, 6, 0x01, 0, 17, 0, 1, 0xff]);
                p.extend_from_slice(&[16, 0, 0x07, 0xd0, 0, 0, 2]);
                s.write_all(&p).await.unwrap();
            }
        }
    });
    port
}

/// Logs in to the fake server; returns whether `Client::connect` finished
/// before `limit`.
fn login(
    rt: &tokio::runtime::Runtime,
    auth: AuthMethod,
    app: Option<String>,
    limit: Duration,
) -> bool {
    rt.block_on(async {
        let port = fake_server().await;
        let mut config = Config::new();
        config.host("127.0.0.1");
        config.port(port);
        config.encryption(EncryptionLevel::NotSupported);
        config.authentication(auth);
        if let Some(app) = app {
            config.application_name(app);
            config.database("d".repeat(100));
        }
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        // The login fails; only what was freed matters.
        tokio::time::timeout(limit, Client::connect(config, tcp.compat_write()))
            .await
            .is_ok()
    })
}

#[test]
fn credentials_are_zeroized_before_being_freed() {
    const PW: &str = "ZqxPasswordMarker!9917";
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let cs = format!("server=tcp:localhost,1433;user=sa;password={PW};database=x");
    let (hits, _) = leaks(PW, || drop(Config::from_ado_string(&cs).unwrap()));
    assert_eq!(hits, 0, "connection string parsing");

    let long = Duration::from_secs(10);
    let (hits, done) = leaks(PW, || {
        login(&rt, AuthMethod::sql_server("sa", PW), None, long)
    });
    assert!(done);
    assert_eq!(hits, 0, "SQL login");

    // LOGIN7 over 512 bytes.
    let (hits, done) = leaks(PW, || {
        let auth = AuthMethod::sql_server("sa", PW);
        login(&rt, auth, Some("a".repeat(128)), long)
    });
    assert!(done);
    assert_eq!(hits, 0, "SQL login with long fields");

    const TOKEN: &str = "eyJAADTOKENMARKER";
    let token = format!("{TOKEN}{}", "x".repeat(2000));

    let auth = AuthMethod::aad_token(&token);
    let (hits, done) = leaks(TOKEN, || login(&rt, auth, None, long));
    assert!(done);
    assert_eq!(hits, 0, "AAD token login");
}
