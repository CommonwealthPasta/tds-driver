use crate::sql_read_bytes::SqlReadBytes;
use futures_util::io::AsyncReadExt;

/// Appends exactly `n` bytes from `src` to `data`, growing `data` in bounded
/// steps so an untrusted length can't force one huge allocation up front.
///
/// `Connection::poll_read` spans packet boundaries itself, so `read_exact`
/// only sees EOF at the end of the stream.
pub(crate) async fn read_into<R>(src: &mut R, data: &mut Vec<u8>, mut n: usize) -> crate::Result<()>
where
    R: SqlReadBytes + Unpin,
{
    const STEP: usize = 64 * 1024;

    while n > 0 {
        let step = n.min(STEP);
        let start = data.len();
        data.resize(start + step, 0);
        src.read_exact(&mut data[start..]).await?;
        n -= step;
    }

    Ok(())
}

/// Reads exactly `n` bytes from `src`.
pub(crate) async fn read_bytes<R>(src: &mut R, n: usize) -> crate::Result<Vec<u8>>
where
    R: SqlReadBytes + Unpin,
{
    let mut data = Vec::with_capacity(n.min(super::MAX_PREALLOC));
    read_into(src, &mut data, n).await?;
    Ok(data)
}

// Decode a partially length-prefixed type.
pub(crate) async fn decode<R>(src: &mut R, len: usize) -> crate::Result<Option<Vec<u8>>>
where
    R: SqlReadBytes + Unpin,
{
    match len {
        // Fixed size
        len if len < 0xffff => {
            let len = src.read_u16_le().await? as usize;

            match len {
                // NULL
                0xffff => Ok(None),
                _ => Ok(Some(read_bytes(src, len).await?)),
            }
        }
        // Unknown size, length-prefixed blobs
        _ => {
            let len = src.read_u64_le().await?;

            let mut data = match len {
                // NULL
                0xffffffffffffffff => return Ok(None),
                // Unknown size
                0xfffffffffffffffe => Vec::new(),
                // Known size. `len` is an untrusted 64-bit wire value; cap the
                // up-front reservation (avoids memory-exhaustion and the
                // `Vec` capacity-overflow panic for values near u64::MAX).
                _ => Vec::with_capacity((len as usize).min(super::MAX_PREALLOC)),
            };

            loop {
                let chunk_size = src.read_u32_le().await? as usize;

                if chunk_size == 0 {
                    break; // found a sentinel, we're done
                }

                // The number of chunks in an "unknown length" PLP value is
                // unbounded on the wire. Cap the running total so a hostile
                // server cannot stream chunks forever and exhaust memory on
                // a single value.
                if data.len().saturating_add(chunk_size) > super::MAX_PLP_SIZE {
                    return Err(crate::Error::Protocol(
                        format!(
                            "PLP value exceeds the maximum supported size of {} bytes",
                            super::MAX_PLP_SIZE
                        )
                        .into(),
                    ));
                }

                read_into(src, &mut data, chunk_size).await?;
            }

            Ok(Some(data))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
    use bytes::{BufMut, BytesMut};

    // The `len` argument selects the wire layout: `len < 0xffff` means a plain
    // `u16`-prefixed value, otherwise a `u64`-prefixed chunked (PLP) value. At
    // the boundary `len == 0xffff` the real code takes the chunked branch
    // (reads a `u64` length). Mutating `<` to `<=` would take the fixed branch
    // (reads a `u16` length) and produce a different value. We feed a chunked
    // stream that decodes to [0xAA, 0xBB, 0xCC]; under the `<=` mutant the same
    // bytes are misread as a `u16` length of 5 followed by five zero bytes.
    #[tokio::test]
    async fn decode_boundary_len_uses_chunked_branch() {
        let mut buf = BytesMut::new();
        buf.put_u64_le(5); // known-size PLP total length
        buf.put_u32_le(3); // chunk size
        buf.put_slice(&[0xAA, 0xBB, 0xCC]);
        buf.put_u32_le(0); // terminating chunk

        let data = decode(&mut buf.into_sql_read_bytes(), 0xffff)
            .await
            .unwrap();
        assert_eq!(data, Some(vec![0xAA, 0xBB, 0xCC]));
    }

    // A chunk whose running total strictly exceeds `MAX_PLP_SIZE` must be
    // rejected with the "exceeds the maximum" protocol error *before* any chunk
    // data is read. Mutating `>` to `==` would let a strictly-greater total slip
    // past the guard (the `==` only fires at the exact boundary), so instead of
    // the protocol error the decoder would try to read a ~4 GiB chunk and fail
    // with an unrelated read error.
    #[tokio::test]
    async fn decode_oversized_chunk_is_rejected() {
        let mut buf = BytesMut::new();
        buf.put_u64_le(10); // known-size marker -> chunked branch bookkeeping
        buf.put_u32_le(u32::MAX); // chunk size well past MAX_PLP_SIZE

        let err = decode(&mut buf.into_sql_read_bytes(), 0x10000)
            .await
            .expect_err("oversized PLP chunk must be rejected");

        match err {
            crate::Error::Protocol(msg) => {
                assert!(
                    msg.contains("exceeds the maximum"),
                    "unexpected protocol message: {msg}"
                );
            }
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }
}
