//! No heap block freed while parsing a connection string may still hold a
//! credential: plaintext, UTF-16 or LOGIN7-obfuscated. A scanning global
//! allocator checks every block as it is freed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use tiberius::Config;

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

#[test]
fn credentials_are_zeroized_before_being_freed() {
    const PW: &str = "ZqxPasswordMarker!9917";

    let cs = format!("server=tcp:localhost,1433;user=sa;password={PW};database=x");
    let (hits, _) = leaks(PW, || drop(Config::from_ado_string(&cs).unwrap()));
    assert_eq!(hits, 0, "connection string parsing");
}
