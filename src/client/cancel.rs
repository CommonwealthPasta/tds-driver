use futures_util::task::AtomicWaker;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::Context;

/// Cancels the request in flight on a [`Client`]'s connection from outside the
/// future awaiting it: another task, or a `select!` branch beside it.
///
/// Unlike [`Client::cancel_query`], which can only run once the response
/// stream has been dropped, this also stops a request that is still being sent
/// or that the server hasn't answered yet, such as a statement still sorting,
/// aggregating or waiting on a lock.
///
/// [`Client`]: crate::Client
/// [`Client::cancel_query`]: crate::Client::cancel_query
#[derive(Clone, Debug, Default)]
pub struct CancelHandle(Arc<Signal>);

#[derive(Debug, Default)]
struct Signal {
    requested: AtomicBool,
    waker: AtomicWaker,
}

impl CancelHandle {
    /// Asks the server to stop the request in flight on the connection.
    ///
    /// The Attention goes out the next time the response is read, so keep
    /// polling the future awaiting it. It then finishes early, with whatever
    /// the server sent before it stopped, or with server error 3980 if the
    /// request was aborted before it ran.
    ///
    /// A cancel is forgotten if it comes before a request is sent, so it never
    /// stops a later one, or if the response is dropped before the Attention
    /// goes out; call [`Client::cancel_query`] then instead.
    ///
    /// [`Client::cancel_query`]: crate::Client::cancel_query
    pub fn cancel(&self) {
        self.0.requested.store(true, Ordering::Release);
        self.0.waker.wake();
    }

    /// Whether a cancel is pending. Registers `cx` to be woken by the next one.
    pub(crate) fn poll_requested(&self, cx: &mut Context<'_>) -> bool {
        self.0.waker.register(cx.waker());
        self.0.requested.load(Ordering::Acquire)
    }

    /// Forgets a pending cancel.
    pub(crate) fn clear(&self) {
        self.0.requested.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Wake, Waker};

    struct CountWakes(AtomicUsize);

    impl Wake for CountWakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn cancel_wakes_the_reader_and_stays_pending_until_cleared() {
        let wakes = Arc::new(CountWakes(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&wakes));
        let mut cx = Context::from_waker(&waker);
        let handle = CancelHandle::default();

        assert!(!handle.poll_requested(&mut cx));
        handle.clone().cancel();
        assert_eq!(1, wakes.0.load(Ordering::SeqCst));
        assert!(handle.poll_requested(&mut cx));

        handle.clear();
        assert!(!handle.poll_requested(&mut cx));
    }
}
