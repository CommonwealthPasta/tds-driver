mod batch_request;
mod bulk_load;
mod column_data;
mod decode;
mod encode;
mod guid;
mod header;
mod iterator_ext;
mod login;
mod packet;
mod pre_login;
mod rpc_request;
mod token;
mod transaction_manager;
mod type_info;
mod type_info_tvp;

pub use batch_request::*;
pub use bulk_load::*;
use bytes::BytesMut;
pub use column_data::*;
pub use decode::*;
pub(crate) use encode::*;
use futures_util::stream::{Stream, TryStreamExt};
pub use header::*;
pub(crate) use iterator_ext::*;
pub use login::*;
pub use packet::*;
pub use pre_login::*;
pub use rpc_request::*;
pub use token::*;
pub use transaction_manager::*;
pub use type_info::*;
pub use type_info_tvp::*;

const HEADER_BYTES: usize = 8;
const ALL_HEADERS_LEN_TX: usize = 22;

#[derive(Debug)]
#[repr(u16)]
#[allow(dead_code)]
enum AllHeaderTy {
    QueryDescriptor = 1,
    TransactionDescriptor = 2,
    TraceActivity = 3,
}

pub struct PacketCodec;

/// The longest message [`collect_from`] takes. Its one use is the PRELOGIN
/// answer, which SqlClient reads from a single packet of at most 32768 bytes
/// (`TdsParser.ConsumePreLoginHandshake`); without a cap a server could grow
/// the buffer without end.
const MAX_COLLECTED: usize = 32768;

pub(crate) async fn collect_from<S, T>(stream: &mut S) -> crate::Result<T>
where
    T: Decode<BytesMut> + Sized,
    S: Stream<Item = crate::Result<Packet>> + Unpin,
{
    let mut buf = BytesMut::new();

    while let Some(packet) = stream.try_next().await? {
        let is_last = packet.is_last();
        let (_, payload) = packet.into_parts();
        if buf.len() + payload.len() > MAX_COLLECTED {
            return Err(crate::Error::Protocol(
                format!("a message over {MAX_COLLECTED} bytes where one packet was expected")
                    .into(),
            ));
        }
        buf.extend(payload);

        if is_last {
            break;
        }
    }

    T::decode(&mut buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn packet(payload: &[u8], last: bool) -> crate::Result<Packet> {
        let mut header = PacketHeader::pre_login(0);
        header.set_status(match last {
            true => PacketStatus::EndOfMessage,
            false => PacketStatus::NormalMessage,
        });
        Ok(Packet::new(header, BytesMut::from(payload)))
    }

    /// A PRELOGIN answer that is only its terminator, padded to `len` bytes.
    async fn collect(len: usize) -> crate::Result<PreloginMessage> {
        let mut packets = vec![packet(&[0xff], false)];
        let pad = vec![0; 4096];
        let mut rest = len - 1;
        while rest > 0 {
            let n = rest.min(pad.len());
            packets.push(packet(&pad[..n], false));
            rest -= n;
        }
        packets.push(packet(&[], true));
        collect_from(&mut stream::iter(packets)).await
    }

    #[tokio::test]
    async fn collect_from_caps_the_message() {
        assert!(collect(MAX_COLLECTED).await.is_ok());
        match collect(MAX_COLLECTED + 1).await {
            Err(crate::Error::Protocol(msg)) => assert!(msg.contains("32768"), "{msg}"),
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }
}
