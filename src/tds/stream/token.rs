use crate::tds::codec::TokenSspi;
use crate::{
    client::Connection,
    tds::codec::{
        RelatedMessages, TokenAltMetaData, TokenAltRow, TokenColInfo, TokenColMetaData, TokenDone,
        TokenEnvChange, TokenError, TokenFeatureExtAck, TokenFedAuthInfo, TokenInfo, TokenLoginAck,
        TokenOrder, TokenReturnValue, TokenRow, TokenSessionState, TokenTabName,
    },
    Error, SqlReadBytes, TokenType,
};
use futures_util::{
    io::{AsyncRead, AsyncWrite},
    stream::{BoxStream, TryStreamExt},
};
use std::{convert::TryFrom, sync::Arc};
use tracing::{event, Level};

#[derive(Debug)]
#[allow(dead_code)]
pub enum ReceivedToken {
    NewResultset(Arc<TokenColMetaData<'static>>),
    NewAltResultset(Arc<TokenAltMetaData<'static>>),
    Row(TokenRow<'static>),
    AltRow(TokenAltRow<'static>),
    Done(TokenDone),
    DoneInProc(TokenDone),
    DoneProc(TokenDone),
    ReturnStatus(i32),
    ReturnValue(TokenReturnValue),
    Order(TokenOrder),
    ColInfo(TokenColInfo),
    TabName(TokenTabName),
    EnvChange(TokenEnvChange),
    Info(TokenInfo),
    LoginAck(TokenLoginAck),
    Sspi(TokenSspi),
    FeatureExtAck(TokenFeatureExtAck),
    SessionState(TokenSessionState),
    FedAuthInfo(TokenFedAuthInfo),
    Error(TokenError),
}

impl ReceivedToken {
    /// See [`TokenDone::records_affected`].
    pub(crate) fn records_affected(&self) -> Option<u64> {
        match self {
            ReceivedToken::Done(done)
            | ReceivedToken::DoneProc(done)
            | ReceivedToken::DoneInProc(done) => done.records_affected(),
            _ => None,
        }
    }
}

/// SqlClient keeps every message; the cap stops a `PRINT` loop in a long
/// batch from growing memory without bound.
const MAX_RELATED_MESSAGES: usize = 1000;

/// The ERROR and INFO tokens of one response. The first ERROR is what the
/// request fails with; the others are attached to it.
#[derive(Debug, Default)]
struct ServerMessages {
    first_error: Option<TokenError>,
    related: RelatedMessages,
}

impl ServerMessages {
    fn error(&mut self, error: TokenError) {
        if self.first_error.is_none() {
            self.first_error = Some(error);
        } else if self.related.errors.len() < MAX_RELATED_MESSAGES {
            self.related.errors.push(error);
        }
    }

    fn info(&mut self, info: &TokenInfo) {
        if self.related.infos.len() < MAX_RELATED_MESSAGES {
            self.related.infos.push(info.into());
        }
    }

    fn into_error(self) -> Option<Error> {
        let mut error = self.first_error?;
        if !self.related.is_empty() {
            error.related = Some(Box::new(self.related));
        }
        Some(Error::Server(error))
    }
}

/// Decodes LOGINACK and records the negotiated TDS version, which later
/// DONE, ERROR and INFO tokens are laid out by.
async fn decode_login_ack<R>(src: &mut R) -> crate::Result<TokenLoginAck>
where
    R: SqlReadBytes + Unpin,
{
    let ack = TokenLoginAck::decode(src).await?;
    src.context_mut().set_version(ack.tds_version);
    Ok(ack)
}

pub(crate) struct TokenStream<'a, S: AsyncRead + AsyncWrite + Unpin + Send> {
    conn: &'a mut Connection<S>,
    /// Boxed on the first ERROR or INFO: the stream moves on every token, so
    /// it stays two words.
    messages: Option<Box<ServerMessages>>,
}

impl<'a, S> TokenStream<'a, S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    pub(crate) fn new(conn: &'a mut Connection<S>) -> Self {
        Self {
            conn,
            messages: None,
        }
    }

    pub(crate) async fn flush_done(mut self) -> crate::Result<TokenDone> {
        let mut routing = None;

        loop {
            match self.next_token().await? {
                Some(ReceivedToken::Done(token)) => {
                    match (self.messages.take().and_then(|m| m.into_error()), routing) {
                        (Some(error), _) => return Err(error),
                        (_, Some(routing)) => return Err(routing),
                        (_, _) => return Ok(token),
                    }
                }
                Some(ReceivedToken::EnvChange(TokenEnvChange::Routing { host, port })) => {
                    routing = Some(Error::Routing { host, port });
                }
                Some(_) => (),
                None => return Err(crate::Error::Protocol("Never got DONE token.".into())),
            }
        }
    }

    /// Drain the token stream after a client Attention signal has been sent,
    /// discarding every remaining token of the cancelled request until the
    /// acknowledging DONE token (with the `DONE_ATTN` status bit set) is
    /// received, per MS-TDS section 2.2.1.7. Returns that DONE token, or
    /// `None` if the current message ends first.
    pub(crate) async fn flush_done_attention(self) -> crate::Result<Option<TokenDone>> {
        let mut stream = self.try_unfold();

        loop {
            match stream.try_next().await? {
                Some(ReceivedToken::Done(token))
                | Some(ReceivedToken::DoneProc(token))
                | Some(ReceivedToken::DoneInProc(token))
                    if token.is_attention() =>
                {
                    return Ok(Some(token));
                }
                Some(_) => (),
                None => return Ok(None),
            }
        }
    }

    /// Reads the rest of the response for its effect on the connection, such
    /// as ENVCHANGEs, dropping its results and any server error in it.
    pub(crate) async fn drain(self) -> crate::Result<()> {
        let mut stream = self.try_unfold();

        loop {
            match stream.try_next().await {
                Ok(Some(_)) => (),
                // A server error surfaces at the end of the response.
                Ok(None) | Err(Error::Server(_)) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    #[cfg(any(
        all(windows, feature = "winauth"),
        feature = "integrated-auth-gssapi",
        feature = "sspi-rs"
    ))]
    pub(crate) async fn flush_sspi(self) -> crate::Result<TokenSspi> {
        let mut stream = self.try_unfold();
        let mut messages = ServerMessages::default();

        loop {
            match stream.try_next().await? {
                Some(ReceivedToken::Error(error)) => messages.error(error),
                Some(ReceivedToken::Info(info)) => messages.info(&info),
                Some(ReceivedToken::Sspi(token)) => return Ok(token),
                Some(_) => (),
                None => match messages.into_error() {
                    Some(err) => return Err(err),
                    None => return Err(crate::Error::Protocol("Never got SSPI token.".into())),
                },
            }
        }
    }

    async fn get_col_metadata(&mut self) -> crate::Result<ReceivedToken> {
        let meta = Arc::new(TokenColMetaData::decode(self.conn).await?);
        self.conn.context_mut().set_last_meta(meta.clone());

        event!(Level::TRACE, ?meta);

        Ok(ReceivedToken::NewResultset(meta))
    }

    async fn get_alt_col_metadata(&mut self) -> crate::Result<ReceivedToken> {
        let meta = Arc::new(TokenAltMetaData::decode(self.conn).await?);
        self.conn.context_mut().set_alt_meta(meta.clone());

        event!(Level::TRACE, ?meta);

        Ok(ReceivedToken::NewAltResultset(meta))
    }

    async fn get_alt_row(&mut self) -> crate::Result<ReceivedToken> {
        // The id is read first so the matching ALTMETADATA can be looked up
        // before the column values are parsed.
        let id = self.conn.read_u16_le().await?;

        let meta = self.conn.context().alt_meta(id).ok_or_else(|| {
            Error::Protocol(format!("ALTROW for unknown compute id {}", id).into())
        })?;

        let row = TokenAltRow::decode(self.conn, id, &meta).await?;

        event!(Level::TRACE, message = ?row);
        Ok(ReceivedToken::AltRow(row))
    }

    async fn get_row(&mut self) -> crate::Result<ReceivedToken> {
        let return_value = TokenRow::decode(self.conn).await?;

        event!(Level::TRACE, message = ?return_value);
        Ok(ReceivedToken::Row(return_value))
    }

    async fn get_nbc_row(&mut self) -> crate::Result<ReceivedToken> {
        let return_value = TokenRow::decode_nbc(self.conn).await?;

        event!(Level::TRACE, message = ?return_value);
        Ok(ReceivedToken::Row(return_value))
    }

    async fn get_return_value(&mut self) -> crate::Result<ReceivedToken> {
        let return_value = TokenReturnValue::decode(self.conn).await?;
        event!(Level::TRACE, message = ?return_value);
        Ok(ReceivedToken::ReturnValue(return_value))
    }

    async fn get_return_status(&mut self) -> crate::Result<ReceivedToken> {
        // MS-TDS 2.2.7.18: a LONG, signed (`RETURN -1`).
        let status = self.conn.read_i32_le().await?;
        Ok(ReceivedToken::ReturnStatus(status))
    }

    async fn get_error(&mut self) -> crate::Result<ReceivedToken> {
        let err = TokenError::decode(self.conn).await?;
        self.messages.get_or_insert_default().error(err.clone());

        event!(Level::ERROR, message = %err.message, code = err.code);
        Ok(ReceivedToken::Error(err))
    }

    async fn get_order(&mut self) -> crate::Result<ReceivedToken> {
        let order = TokenOrder::decode(self.conn).await?;
        event!(Level::TRACE, message = ?order);
        Ok(ReceivedToken::Order(order))
    }

    async fn get_col_info(&mut self) -> crate::Result<ReceivedToken> {
        let col_info = TokenColInfo::decode(self.conn).await?;
        event!(Level::TRACE, message = ?col_info);
        Ok(ReceivedToken::ColInfo(col_info))
    }

    async fn get_tab_name(&mut self) -> crate::Result<ReceivedToken> {
        let tab_name = TokenTabName::decode(self.conn).await?;
        event!(Level::TRACE, message = ?tab_name);
        Ok(ReceivedToken::TabName(tab_name))
    }

    async fn get_done_value(&mut self) -> crate::Result<ReceivedToken> {
        let done = TokenDone::decode(self.conn).await?;
        event!(Level::TRACE, "{}", done);
        Ok(ReceivedToken::Done(done))
    }

    async fn get_done_proc_value(&mut self) -> crate::Result<ReceivedToken> {
        let done = TokenDone::decode(self.conn).await?;
        event!(Level::TRACE, "{}", done);
        Ok(ReceivedToken::DoneProc(done))
    }

    async fn get_done_in_proc_value(&mut self) -> crate::Result<ReceivedToken> {
        let done = TokenDone::decode(self.conn).await?;
        event!(Level::TRACE, "{}", done);
        Ok(ReceivedToken::DoneInProc(done))
    }

    async fn get_env_change(&mut self) -> crate::Result<ReceivedToken> {
        let change = TokenEnvChange::decode(self.conn).await?;

        match change {
            TokenEnvChange::PacketSize(new_size, _) => {
                // MS-TDS: a negotiated packet size must be within 512..=32767.
                // Reject an out-of-range value from the server: the subsequent
                // `packet_size - HEADER_BYTES` in the send paths would otherwise
                // underflow (debug panic) or, for a value of exactly 8, produce
                // a zero chunk size and spin `split_to(0)` forever.
                if !(512..=32767).contains(&new_size) {
                    return Err(crate::Error::Protocol(
                        format!("server requested an invalid packet size of {new_size}").into(),
                    ));
                }
                self.conn.context_mut().set_packet_size(new_size);
            }
            TokenEnvChange::BeginTransaction(desc) => {
                self.conn.context_mut().set_transaction_descriptor(desc);
            }
            TokenEnvChange::CommitTransaction
            | TokenEnvChange::RollbackTransaction
            | TokenEnvChange::DefectTransaction => {
                self.conn.context_mut().set_transaction_descriptor([0; 8]);
            }
            TokenEnvChange::SqlCollation { new, .. } => {
                self.conn.context_mut().set_collation(new);
            }
            _ => (),
        }

        event!(Level::DEBUG, "{}", change);

        Ok(ReceivedToken::EnvChange(change))
    }

    async fn get_info(&mut self) -> crate::Result<ReceivedToken> {
        let info = TokenInfo::decode(self.conn).await?;
        self.messages.get_or_insert_default().info(&info);
        event!(Level::DEBUG, "{}", info.message);
        Ok(ReceivedToken::Info(info))
    }

    async fn get_login_ack(&mut self) -> crate::Result<ReceivedToken> {
        let ack = decode_login_ack(self.conn).await?;
        event!(Level::DEBUG, "{} version {}", ack.prog_name, ack.version);
        Ok(ReceivedToken::LoginAck(ack))
    }

    async fn get_feature_ext_ack(&mut self) -> crate::Result<ReceivedToken> {
        let ack = TokenFeatureExtAck::decode(self.conn).await?;
        event!(
            Level::DEBUG,
            "FeatureExtAck with {} features",
            ack.features.len()
        );
        Ok(ReceivedToken::FeatureExtAck(ack))
    }

    async fn get_session_state(&mut self) -> crate::Result<ReceivedToken> {
        let state = TokenSessionState::decode(self.conn).await?;
        event!(
            Level::TRACE,
            "SessionState seq_no={} recoverable={} states={}",
            state.seq_no,
            state.is_recoverable(),
            state.states.len()
        );
        Ok(ReceivedToken::SessionState(state))
    }

    async fn get_fed_auth_info(&mut self) -> crate::Result<ReceivedToken> {
        let info = TokenFedAuthInfo::decode(self.conn).await?;
        event!(Level::TRACE, message = ?info);
        Ok(ReceivedToken::FedAuthInfo(info))
    }

    async fn get_sspi(&mut self) -> crate::Result<ReceivedToken> {
        let sspi = TokenSspi::decode_async(self.conn).await?;
        event!(Level::TRACE, "SSPI response");
        Ok(ReceivedToken::Sspi(sspi))
    }

    pub fn try_unfold(self) -> BoxStream<'a, crate::Result<ReceivedToken>> {
        let stream = futures_util::stream::try_unfold(self, |mut this| async move {
            Ok(this.next_token().await?.map(|token| (token, this)))
        });

        Box::pin(stream)
    }

    /// The next token, or at the end of the response `None`, or the first
    /// server error in it.
    async fn next_token(&mut self) -> crate::Result<Option<ReceivedToken>> {
        if self.conn.is_eof() {
            return match self.messages.take().and_then(|m| m.into_error()) {
                None => Ok(None),
                Some(error) => Err(error),
            };
        }

        self.conn.begin_token()?;
        let token = match self.read_token().await {
            Ok(token) => token,
            Err(e) => {
                self.conn.abandon_token();
                return Err(e);
            }
        };
        self.conn.end_token();

        if let ReceivedToken::Done(done)
        | ReceivedToken::DoneProc(done)
        | ReceivedToken::DoneInProc(done) = &token
        {
            if done.is_attention() {
                self.conn.attention_acknowledged();
            }
        }

        Ok(Some(token))
    }

    async fn read_token(&mut self) -> crate::Result<ReceivedToken> {
        let ty_byte = self.conn.read_u8().await?;

        let ty = TokenType::try_from(ty_byte)
            .map_err(|_| Error::Protocol(format!("invalid token type {:x}", ty_byte).into()))?;

        let token = match ty {
            TokenType::ReturnStatus => self.get_return_status().await?,
            TokenType::ColMetaData => self.get_col_metadata().await?,
            TokenType::AltMetaData => self.get_alt_col_metadata().await?,
            TokenType::Row => self.get_row().await?,
            TokenType::AltRow => self.get_alt_row().await?,
            TokenType::NbcRow => self.get_nbc_row().await?,
            TokenType::Done => self.get_done_value().await?,
            TokenType::DoneProc => self.get_done_proc_value().await?,
            TokenType::DoneInProc => self.get_done_in_proc_value().await?,
            TokenType::ReturnValue => self.get_return_value().await?,
            TokenType::Error => self.get_error().await?,
            TokenType::Order => self.get_order().await?,
            TokenType::ColInfo => self.get_col_info().await?,
            TokenType::TabName => self.get_tab_name().await?,
            TokenType::EnvChange => self.get_env_change().await?,
            TokenType::Info => self.get_info().await?,
            TokenType::LoginAck => self.get_login_ack().await?,
            TokenType::Sspi => self.get_sspi().await?,
            TokenType::SessionState => self.get_session_state().await?,
            TokenType::FedAuthInfo => self.get_fed_auth_info().await?,
            TokenType::FeatureExtAck => self.get_feature_ext_ack().await?,
            // Defensive fallback for any token type without a dedicated
            // handler. Currently every variant is handled, so this is
            // unreachable, but it is kept as new token types are added.
            #[allow(unreachable_patterns)]
            _ => {
                return Err(Error::Protocol(
                    format!("Token {:?} unimplemented!", ty).into(),
                ))
            }
        };

        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
    use crate::tds::codec::{DoneStatus, FeatureLevel};
    use bytes::{BufMut, BytesMut};

    fn error(code: u32, message: &str) -> TokenError {
        TokenError {
            code,
            state: 1,
            class: 16,
            message: message.into(),
            server: "".into(),
            procedure: "".into(),
            line: 1,
            related: None,
        }
    }

    fn info(number: u32, message: &str) -> TokenInfo {
        TokenInfo {
            number,
            state: 1,
            class: 0,
            message: message.into(),
            server: "".into(),
            procedure: "".into(),
            line: 1,
        }
    }

    #[test]
    fn server_messages_keep_every_error_and_info() {
        let mut messages = ServerMessages::default();
        messages.info(&info(0, "before"));
        messages.error(error(50000, "first"));
        messages.error(error(50001, "second"));
        messages.info(&info(0, "after"));

        let Some(Error::Server(err)) = messages.into_error() else {
            panic!("expected a server error");
        };
        assert_eq!((err.code(), err.message()), (50000, "first"));
        let others: Vec<_> = err.other_errors().iter().map(|e| e.message()).collect();
        assert_eq!(others, ["second"]);
        let infos: Vec<_> = err.info_messages().iter().map(|e| e.message()).collect();
        assert_eq!(infos, ["before", "after"]);
    }

    #[test]
    fn server_messages_without_error_are_not_an_error() {
        let mut messages = ServerMessages::default();
        messages.info(&info(5701, "Changed database context"));
        assert!(messages.into_error().is_none());
    }

    #[test]
    fn server_messages_lone_error_has_nothing_attached() {
        let mut messages = ServerMessages::default();
        messages.error(error(208, "Invalid object name"));
        assert_eq!(
            messages.into_error(),
            Some(Error::Server(error(208, "Invalid object name")))
        );
    }

    #[test]
    fn server_messages_are_capped() {
        let mut messages = ServerMessages::default();
        for i in 0..=MAX_RELATED_MESSAGES as u32 + 1 {
            messages.error(error(i, "e"));
            messages.info(&info(i, "i"));
        }
        let Some(Error::Server(err)) = messages.into_error() else {
            panic!("expected a server error");
        };
        assert_eq!(err.other_errors().len(), MAX_RELATED_MESSAGES);
        assert_eq!(err.info_messages().len(), MAX_RELATED_MESSAGES);
    }

    #[tokio::test]
    async fn login_ack_version_sets_done_row_count_width() {
        let mut buf = BytesMut::new();
        buf.put_u16_le(0); // LOGINACK length, ignored
        buf.put_u8(1);
        buf.put_u32(FeatureLevel::SqlServer2000Sp1 as u32);
        buf.put_u8(0); // empty ProgName
        buf.put_u32_le(0x0800_07D0);
        // TDS 7.1 DONE: 4-byte DoneRowCount.
        buf.put_u16_le(DoneStatus::Count as u16);
        buf.put_u16_le(0xC3);
        buf.put_u32_le(7);
        // What a 7.2+ decode would take as the high half of the count.
        buf.put_u32_le(0xFFFF_FFFF);

        let mut src = buf.into_sql_read_bytes();
        let ack = decode_login_ack(&mut src).await.unwrap();
        assert_eq!(ack.tds_version, FeatureLevel::SqlServer2000Sp1);
        assert_eq!(src.context().version(), FeatureLevel::SqlServer2000Sp1);

        let done = TokenDone::decode(&mut src).await.unwrap();
        assert_eq!(done.records_affected(), Some(7));
    }
}
