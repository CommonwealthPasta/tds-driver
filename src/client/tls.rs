#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
use super::tls_stream::TlsStream;
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
use crate::tds::{
    codec::{Decode, Encode, PacketHeader, PacketStatus, PacketType},
    HEADER_BYTES,
};
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
use bytes::BytesMut;
use futures_util::io::{AsyncRead, AsyncWrite};
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
use futures_util::ready;
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
use std::cmp;
use std::{
    io,
    pin::Pin,
    task::{self, Poll},
};
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
use tracing::{event, Level};

/// A wrapper to handle either TLS or bare connections.
pub(crate) enum MaybeTlsStream<S: AsyncRead + AsyncWrite + Unpin + Send> {
    Raw(S),
    #[cfg(any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    ))]
    /// With the bytes of the TDS packet being written that the TLS stream
    /// has yet to take.
    Tls(TlsStream<TlsPreloginWrapper<S>>, usize),
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> MaybeTlsStream<S> {
    pub fn into_inner(self) -> S {
        match self {
            Self::Raw(s) => s,
            #[cfg(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            ))]
            Self::Tls(mut tls, _) => tls.get_mut().stream.take().unwrap(),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> AsyncRead for MaybeTlsStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            MaybeTlsStream::Raw(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            ))]
            MaybeTlsStream::Tls(s, _) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> AsyncWrite for MaybeTlsStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            MaybeTlsStream::Raw(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            ))]
            MaybeTlsStream::Tls(s, left) => {
                // SQL Server drops the connection when a TLS record holds
                // parts of two TDS packets, so the TLS stream gets at most
                // the rest of one packet per write.
                if *left == 0 && buf.len() >= HEADER_BYTES {
                    *left = usize::from(u16::from_be_bytes([buf[2], buf[3]])).max(HEADER_BYTES);
                }
                let end = if *left == 0 {
                    buf.len()
                } else {
                    buf.len().min(*left)
                };
                let written = ready!(Pin::new(s).poll_write(cx, &buf[..end]))?;
                *left = left.saturating_sub(written);
                Poll::Ready(Ok(written))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Raw(s) => Pin::new(s).poll_flush(cx),
            #[cfg(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            ))]
            MaybeTlsStream::Tls(s, _) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTlsStream::Raw(s) => Pin::new(s).poll_close(cx),
            #[cfg(any(
                feature = "rustls",
                feature = "native-tls",
                feature = "vendored-openssl"
            ))]
            MaybeTlsStream::Tls(s, _) => Pin::new(s).poll_close(cx),
        }
    }
}

/// TDS 7.x carries the TLS handshake in PRELOGIN packets; this wrapper adds
/// and strips that framing until `handshake_complete`, then passes through.
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
pub(crate) struct TlsPreloginWrapper<S> {
    stream: Option<S>,
    pending_handshake: bool,
    /// The handshake travels in PRELOGIN packets (TDS 7.x, not strict).
    framed: bool,

    header_buf: [u8; HEADER_BYTES],
    header_pos: usize,
    read_remaining: usize,

    /// TLS bytes written but not yet framed.
    wr_buf: Vec<u8>,
    /// Bytes for the wire, of which `out_pos` are on it: framed handshake
    /// packets, then TLS records gathered into fewer, larger socket writes.
    out: Vec<u8>,
    out_pos: usize,
    packet_id: u8,
}

/// Packet size before login negotiates one; SQL Server drops larger
/// handshake packets.
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
const PRELOGIN_PACKET_SIZE: usize = 4096;

/// How many TLS bytes a write gathers before it goes to the socket.
#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
const MAX_GATHERED: usize = 64 * 1024;

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
impl<S> TlsPreloginWrapper<S> {
    pub fn new(stream: S) -> Self {
        TlsPreloginWrapper {
            stream: Some(stream),
            pending_handshake: true,
            framed: true,

            header_buf: [0u8; HEADER_BYTES],
            header_pos: 0,
            read_remaining: 0,
            wr_buf: Vec::new(),
            out: Vec::new(),
            out_pos: 0,
            packet_id: 1,
        }
    }

    /// For TDS 8.0 strict, whose handshake is raw TLS.
    pub fn unframed(stream: S) -> Self {
        Self {
            framed: false,
            ..Self::new(stream)
        }
    }

    pub fn handshake_complete(&mut self) {
        self.pending_handshake = false;
    }

    fn framing(&self) -> bool {
        self.pending_handshake && self.framed
    }

    /// Writes the framed packets still queued from a flush that would have
    /// blocked.
    fn poll_drain(&mut self, cx: &mut task::Context<'_>) -> Poll<io::Result<()>>
    where
        S: AsyncWrite + Unpin,
    {
        while self.out_pos < self.out.len() {
            let written =
                ready!(Pin::new(self.stream.as_mut().unwrap())
                    .poll_write(cx, &self.out[self.out_pos..]))?;

            if written == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }

            self.out_pos += written;
        }

        Poll::Ready(Ok(()))
    }

    /// Moves `wr_buf` into `out` as one PRELOGIN message of packets no
    /// larger than `PRELOGIN_PACKET_SIZE`.
    fn frame_pending(&mut self) {
        let mut chunks = self
            .wr_buf
            .chunks(PRELOGIN_PACKET_SIZE - HEADER_BYTES)
            .peekable();

        while let Some(chunk) = chunks.next() {
            let mut header = PacketHeader::new(chunk.len() + HEADER_BYTES, self.packet_id);
            header.set_type(PacketType::PreLogin);
            header.set_status(if chunks.peek().is_some() {
                PacketStatus::NormalMessage
            } else {
                PacketStatus::EndOfMessage
            });
            header
                .encode(&mut self.out)
                .expect("encoding a header into a Vec cannot fail");
            self.out.extend_from_slice(chunk);
            self.packet_id = self.packet_id.wrapping_add(1);
        }

        self.wr_buf.clear();
    }
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> AsyncRead for TlsPreloginWrapper<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let inner = self.get_mut();

        // The answer can't come before the flight it answers is on the wire.
        ready!(inner.poll_drain(cx))?;

        // Normal operation does not need any extra treatment, we handle packets
        // in the codec.
        if !inner.framing() {
            return Pin::new(inner.stream.as_mut().unwrap()).poll_read(cx, buf);
        }

        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        // Strip packet headers; an empty packet has no bytes for TLS.
        while inner.read_remaining == 0 {
            while inner.header_pos < HEADER_BYTES {
                let read = ready!(Pin::new(inner.stream.as_mut().unwrap())
                    .poll_read(cx, &mut inner.header_buf[inner.header_pos..]))?;

                if read == 0 {
                    return Poll::Ready(Ok(0));
                }

                inner.header_pos += read;
            }

            let header = PacketHeader::decode(&mut BytesMut::from(&inner.header_buf[..]))
                .map_err(io::Error::other)?;

            // This runs before any certificate has been validated, so the
            // bytes are fully untrusted: reject anything unexpected instead of
            // panicking.
            if header.r#type() != PacketType::PreLogin {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected a pre-login packet during the TLS handshake",
                )));
            }

            inner.read_remaining = (header.length() as usize)
                .checked_sub(HEADER_BYTES)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "pre-login packet length shorter than its header",
                    )
                })?;
            inner.header_pos = 0;

            event!(
                Level::TRACE,
                "Reading packet of {} bytes",
                inner.read_remaining,
            );
        }

        let max_read = cmp::min(inner.read_remaining, buf.len());

        let read = ready!(
            Pin::new(&mut inner.stream.as_mut().unwrap()).poll_read(cx, &mut buf[..max_read])
        )?;

        inner.read_remaining -= read;

        Poll::Ready(Ok(read))
    }
}

#[cfg(any(
    feature = "rustls",
    feature = "native-tls",
    feature = "vendored-openssl"
))]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> AsyncWrite for TlsPreloginWrapper<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // Normal operation does not need any extra treatment, we handle
        // packets in the codec.
        if !self.framing() {
            let inner = self.get_mut();
            if inner.out.len() - inner.out_pos >= MAX_GATHERED {
                ready!(inner.poll_drain(cx))?;
            }
            if inner.out_pos == inner.out.len() {
                inner.out.clear();
                inner.out_pos = 0;
            }
            // Sent on flush, or before the next read.
            inner.out.extend_from_slice(buf);
            return Poll::Ready(Ok(buf.len()));
        }

        // Framed on flush, which is where a TLS flight ends.
        self.wr_buf.extend_from_slice(buf);

        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        let inner = self.get_mut();

        // Bytes written while a flight is partly on the wire become the next
        // message, never the tail of the current one.
        loop {
            match inner.poll_drain(cx) {
                Poll::Ready(result) => result?,
                // OpenSSL fails the handshake on a flush that would block
                // (rust-openssl sets no retry flag for BIO flush). Reporting
                // it done is safe: `poll_read` sends the rest first.
                Poll::Pending if inner.pending_handshake => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }

            if inner.wr_buf.is_empty() {
                break;
            }

            inner.out.clear();
            inner.out_pos = 0;
            inner.frame_pending();

            event!(
                Level::TRACE,
                "Writing {} bytes of pre-login packets",
                inner.out.len(),
            );
        }

        match Pin::new(inner.stream.as_mut().unwrap()).poll_flush(cx) {
            Poll::Pending if inner.pending_handshake => Poll::Ready(Ok(())),
            flushed => flushed,
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        let inner = self.get_mut();
        ready!(inner.poll_drain(cx))?;
        Pin::new(inner.stream.as_mut().unwrap()).poll_close(cx)
    }
}

#[cfg(all(
    test,
    any(
        feature = "rustls",
        feature = "native-tls",
        feature = "vendored-openssl"
    )
))]
mod tests {
    use super::*;
    use std::task::{Context, Waker};

    /// Moves at most `max` bytes per call and returns `Pending` on every
    /// other call.
    struct Choppy {
        input: Vec<u8>,
        read_pos: usize,
        written: Vec<u8>,
        max: usize,
        flip: bool,
    }

    impl Choppy {
        fn new(max: usize, input: Vec<u8>) -> Self {
            Self {
                input,
                read_pos: 0,
                written: Vec::new(),
                max,
                flip: false,
            }
        }

        fn pending(&mut self, cx: &mut Context<'_>) -> bool {
            self.flip = !self.flip;
            if self.flip {
                cx.waker().wake_by_ref();
            }
            self.flip
        }
    }

    impl AsyncRead for Choppy {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            if self.pending(cx) {
                return Poll::Pending;
            }
            let n = buf
                .len()
                .min(self.max)
                .min(self.input.len() - self.read_pos);
            buf[..n].copy_from_slice(&self.input[self.read_pos..self.read_pos + n]);
            self.read_pos += n;
            Poll::Ready(Ok(n))
        }
    }

    impl AsyncWrite for Choppy {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.pending(cx) {
                return Poll::Pending;
            }
            let n = buf.len().min(self.max);
            self.written.extend_from_slice(&buf[..n]);
            Poll::Ready(Ok(n))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn cx() -> Context<'static> {
        Context::from_waker(Waker::noop())
    }

    fn write(w: &mut TlsPreloginWrapper<Choppy>, data: &[u8]) {
        assert_eq!(
            Pin::new(w).poll_write(&mut cx(), data).map(Result::unwrap),
            Poll::Ready(data.len())
        );
    }

    /// Flushes until nothing is queued; mid-handshake, a flush that would
    /// block reports done and leaves the rest queued.
    fn flush(w: &mut TlsPreloginWrapper<Choppy>) {
        loop {
            let flushed = Pin::new(&mut *w).poll_flush(&mut cx()).map(Result::unwrap);
            if flushed.is_ready() && w.out_pos == w.out.len() && w.wr_buf.is_empty() {
                return;
            }
        }
    }

    /// Splits wire bytes into messages, checking every packet header.
    fn messages(mut wire: &[u8]) -> Vec<Vec<u8>> {
        let mut messages = vec![Vec::new()];
        while !wire.is_empty() {
            assert_eq!(wire[0], PacketType::PreLogin as u8);
            let len = u16::from_be_bytes([wire[2], wire[3]]) as usize;
            assert!(
                (HEADER_BYTES + 1..=PRELOGIN_PACKET_SIZE).contains(&len),
                "packet of {len} bytes"
            );
            messages
                .last_mut()
                .unwrap()
                .extend_from_slice(&wire[HEADER_BYTES..len]);
            if wire[1] == PacketStatus::EndOfMessage as u8 {
                messages.push(Vec::new());
            } else {
                assert_eq!(wire[1], PacketStatus::NormalMessage as u8);
            }
            wire = &wire[len..];
        }
        assert!(
            messages.pop().unwrap().is_empty(),
            "last message lacks EndOfMessage"
        );
        messages
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn flights_survive_partial_writes_and_are_split_into_packets() {
        for (len, max) in [(1, 1), (9, 3), (4088, 3), (4089, 7), (70_000, 1000)] {
            let flight = pattern(len, 1);
            let mut w = TlsPreloginWrapper::new(Choppy::new(max, Vec::new()));
            write(&mut w, &flight);
            flush(&mut w);
            assert_eq!(
                messages(&w.stream.as_ref().unwrap().written),
                vec![flight],
                "len {len}"
            );
        }
    }

    #[test]
    fn a_flush_that_would_block_mid_handshake_is_finished_by_the_next_read() {
        let flight = pattern(100, 5);
        let mut w = TlsPreloginWrapper::new(Choppy::new(7, Vec::new()));
        write(&mut w, &flight);

        // The first write is `Pending`; OpenSSL would fail the handshake if
        // the flush were.
        assert!(Pin::new(&mut w).poll_flush(&mut cx()).is_ready());
        assert!(w.stream.as_ref().unwrap().written.is_empty());

        let mut buf = [0u8; 16];
        while Pin::new(&mut w).poll_read(&mut cx(), &mut buf).is_pending() {}
        assert_eq!(messages(&w.stream.as_ref().unwrap().written), vec![flight]);
    }

    #[test]
    fn writes_after_the_handshake_go_out_together_on_flush() {
        let (a, b) = (pattern(5000, 6), pattern(300, 7));
        let mut w = TlsPreloginWrapper::new(Choppy::new(1 << 20, Vec::new()));
        w.handshake_complete();
        write(&mut w, &a);
        write(&mut w, &b);
        assert!(w.stream.as_ref().unwrap().written.is_empty());

        flush(&mut w);
        assert_eq!(w.stream.as_ref().unwrap().written, [a, b].concat());
    }

    #[test]
    fn a_read_sends_gathered_writes_first() {
        let a = pattern(100, 8);
        let mut w = TlsPreloginWrapper::new(Choppy::new(7, vec![1, 2, 3]));
        w.handshake_complete();
        write(&mut w, &a);

        let mut buf = [0u8; 3];
        while Pin::new(&mut w).poll_read(&mut cx(), &mut buf).is_pending() {}
        assert_eq!(w.stream.as_ref().unwrap().written, a);
    }

    #[test]
    fn a_strict_handshake_is_unframed_and_never_blocks_a_flush() {
        let flight = pattern(100, 9);
        let mut w = TlsPreloginWrapper::unframed(Choppy::new(7, Vec::new()));
        write(&mut w, &flight);
        assert!(Pin::new(&mut w).poll_flush(&mut cx()).is_ready());

        let mut buf = [0u8; 1];
        while Pin::new(&mut w).poll_read(&mut cx(), &mut buf).is_pending() {}
        assert_eq!(w.stream.as_ref().unwrap().written, flight);
    }

    #[test]
    fn bytes_written_mid_flush_start_a_new_message() {
        let (a, b) = (pattern(5000, 1), pattern(300, 2));
        let mut w = TlsPreloginWrapper::new(Choppy::new(3, Vec::new()));
        write(&mut w, &a);
        for _ in 0..10 {
            let _ = Pin::new(&mut w).poll_flush(&mut cx());
        }
        write(&mut w, &b);
        flush(&mut w);
        assert_eq!(messages(&w.stream.as_ref().unwrap().written), vec![a, b]);
    }

    #[test]
    fn reads_strip_headers_and_skip_empty_packets() {
        let packet = |status: PacketStatus, payload: &[u8]| {
            let mut header = PacketHeader::new(payload.len() + HEADER_BYTES, 1);
            header.set_type(PacketType::PreLogin);
            header.set_status(status);
            let mut out = Vec::new();
            header.encode(&mut out).unwrap();
            out.extend_from_slice(payload);
            out
        };
        let (a, b) = (pattern(5000, 3), pattern(10, 4));
        let mut wire = packet(PacketStatus::NormalMessage, &a);
        wire.extend(packet(PacketStatus::NormalMessage, &[]));
        wire.extend(packet(PacketStatus::EndOfMessage, &b));

        let mut w = TlsPreloginWrapper::new(Choppy::new(5, wire));
        let mut read = Vec::new();
        let mut buf = [0u8; 64];
        while read.len() < a.len() + b.len() {
            if let Poll::Ready(n) = Pin::new(&mut w).poll_read(&mut cx(), &mut buf) {
                let n = n.unwrap();
                assert_ne!(n, 0, "early EOF after {} bytes", read.len());
                read.extend_from_slice(&buf[..n]);
            }
        }
        assert_eq!(read, [a, b].concat());
    }
}
