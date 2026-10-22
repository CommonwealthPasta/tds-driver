use crate::tds::stream::ReceivedToken;
use crate::{row::ColumnType, Column, Row};
use crate::{ColumnData, CommandResult, ResultMetadata};
use futures_util::{
    ready,
    stream::{BoxStream, Peekable, Stream, StreamExt, TryStreamExt},
};
use std::{
    fmt::Debug,
    pin::Pin,
    sync::Arc,
    task::{self, Poll},
};

/// A `Stream` of [`CommandItem`] values produced by executing a [`Command`].
///
/// Items can be result metadata, rows, a return status, return values (OUT
/// parameters) or a rows-affected count.
///
/// [`Command`]: crate::Command
///
/// # Example
///
/// ```no_run
/// # use std::env;
/// # use tiberius::Config;
/// # use tiberius::{Command, CommandItem};
/// # use futures_util::TryStreamExt;
/// # use tokio_util::compat::TokioAsyncWriteCompatExt;
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let c_str = env::var("TIBERIUS_TEST_CONNECTION_STRING").unwrap_or(
/// #     "server=tcp:localhost,1433;integratedSecurity=true;TrustServerCertificate=true".to_owned(),
/// # );
/// # let config = Config::from_ado_string(&c_str)?;
/// # let tcp = tokio::net::TcpStream::connect(config.get_addr()).await?;
/// # tcp.set_nodelay(true)?;
/// # let mut client = tiberius::Client::connect(config, tcp.compat_write()).await?;
/// let mut cmd = Command::new("dbo.usp_SomeStoredProc");
///
/// cmd.bind_param("@foo", 34i32);
/// cmd.bind_param("@zoo", "the zoo string prm");
/// cmd.bind_out_param("@bar", "bar");
/// let mut stream = cmd.exec(&mut client).await?;
///
/// while let Some(item) = stream.try_next().await? {
///     match item {
///         // our first item is the column data always
///         CommandItem::Metadata(meta) if meta.result_index() == 0 => {
///             // the first result column info can be handled here
///         }
///         // ... and from there on from 0..N rows
///         CommandItem::Row(row) if row.result_index() == 0 => {
///             let var: Option<i32> = row.get(0);
///         }
///         // the second result set returns first another metadata item
///         CommandItem::Metadata(meta) => {
///             // .. handling
///         }
///         // ...and, again, we get rows from the second resultset
///         CommandItem::Row(row) => {
///             let var: Option<i32> = row.get(0);
///         }
///         // check return status (returned always)
///         CommandItem::ReturnStatus(rs) => {
///             // .... do something
///         }
///         // collect OUT parameter values
///         CommandItem::ReturnValue(rv) => {
///             // .... do something, like push to a collection
///         }
///         // get affected row count
///         CommandItem::RowsAffected(ra) => {
///             // .... do something, like push to a collection
///         }
///     }
/// }
/// # Ok(())
/// # }
/// ```
///
pub struct CommandStream<'a> {
    token_stream: Peekable<BoxStream<'a, crate::Result<ReceivedToken>>>,
    columns: Option<Arc<Vec<Column>>>,
    result_set_index: Option<usize>,
}

impl<'a> Debug for CommandStream<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandStream")
            .field(
                "token_stream",
                &"BoxStream<'a, crate::Result<ReceivedToken>>",
            )
            .finish()
    }
}

impl<'a> CommandStream<'a> {
    pub(crate) fn new(token_stream: BoxStream<'a, crate::Result<ReceivedToken>>) -> Self {
        Self {
            token_stream: token_stream.peekable(),
            columns: None,
            result_set_index: None,
        }
    }

    /// Collects all results from the command into memory, in the order they
    /// were produced by the server.
    pub async fn into_command_result(mut self) -> crate::Result<CommandResult> {
        let mut results: Vec<Vec<Row>> = Vec::new();
        let mut result: Option<Vec<Row>> = None;
        let mut return_status = 0;
        let mut return_values = Vec::new();
        let mut rows_affected = Vec::new();

        while let Some(item) = self.try_next().await? {
            match item {
                // Every COLMETADATA opens a result set, including one with no
                // rows, so indexes line up with `ResultMetadata::result_index`.
                CommandItem::Metadata(_) => results.extend(result.replace(Vec::new())),
                CommandItem::Row(row) => result.get_or_insert_with(Vec::new).push(row),
                CommandItem::ReturnStatus(rs) => return_status = rs,
                CommandItem::ReturnValue(rv) => return_values.push(rv),
                CommandItem::RowsAffected(rows) => rows_affected.push(rows),
            }
        }

        if let Some(result) = result {
            results.push(result);
        }

        Ok(CommandResult {
            return_code: return_status,
            return_values,
            query_results: results,
            rows_affected,
        })
    }

    /// Converts the stream into a stream of rows, dropping all other items.
    pub fn into_row_stream(self) -> BoxStream<'a, crate::Result<Row>> {
        let s = self.try_filter_map(|item| async {
            match item {
                CommandItem::Row(row) => Ok(Some(row)),
                _ => Ok(None),
            }
        });

        Box::pin(s)
    }
}

/// A single OUT parameter value returned by a [`Command`].
///
/// [`Command`]: crate::Command
#[derive(Debug)]
pub struct CommandReturnValue {
    pub(crate) name: String,
    pub(crate) ord: u16,
    pub(crate) data: ColumnData<'static>,
}

impl CommandReturnValue {
    /// The name of the OUT parameter this value corresponds to.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The ordinal of the OUT parameter as returned by the server.
    pub fn ordinal(&self) -> u16 {
        self.ord
    }

    /// A reference to the raw column data of the returned value.
    pub fn data(&self) -> &ColumnData<'static> {
        &self.data
    }
}

/// An item produced by a [`CommandStream`].
#[derive(Debug)]
pub enum CommandItem {
    /// A single row of data.
    Row(Row),
    /// Metadata describing the upcoming rows.
    Metadata(ResultMetadata),
    /// The procedure's `RETURN` value. Signed: `RETURN -1` is `-1`.
    ReturnStatus(i32),
    /// A return value, matching an OUT parameter.
    ReturnValue(CommandReturnValue),
    /// The number of rows an INSERT, UPDATE, DELETE or other non-SELECT
    /// statement affected. SELECTs and variable assignments produce none, as
    /// with SqlClient's `ExecuteNonQuery`; see [`ExecuteResult::rows_affected`].
    ///
    /// [`ExecuteResult::rows_affected`]: crate::ExecuteResult::rows_affected
    RowsAffected(u64),
}

impl CommandItem {
    pub(crate) fn metadata(columns: Arc<Vec<Column>>, result_index: usize) -> Self {
        Self::Metadata(ResultMetadata {
            columns,
            result_index,
        })
    }

    /// Returns a reference to the metadata, if the item is of a correct variant.
    pub fn as_metadata(&self) -> Option<&ResultMetadata> {
        match self {
            CommandItem::Metadata(ref metadata) => Some(metadata),
            _ => None,
        }
    }

    /// Returns a reference to the row, if the item is of a correct variant.
    pub fn as_row(&self) -> Option<&Row> {
        match self {
            CommandItem::Row(ref row) => Some(row),
            _ => None,
        }
    }

    /// Returns the metadata, if the item is of a correct variant.
    pub fn into_metadata(self) -> Option<ResultMetadata> {
        match self {
            CommandItem::Metadata(metadata) => Some(metadata),
            _ => None,
        }
    }

    /// Returns the row, if the item is of a correct variant.
    pub fn into_row(self) -> Option<Row> {
        match self {
            CommandItem::Row(row) => Some(row),
            _ => None,
        }
    }

    /// Returns the return status, if the item is of a correct variant.
    pub fn as_return_status(&self) -> Option<i32> {
        match self {
            CommandItem::ReturnStatus(rs) => Some(*rs),
            _ => None,
        }
    }

    /// Returns a reference to the return value, if the item is of a correct variant.
    pub fn as_return_value(&self) -> Option<&CommandReturnValue> {
        match self {
            CommandItem::ReturnValue(rv) => Some(rv),
            _ => None,
        }
    }

    /// Returns the return value, if the item is of a correct variant.
    pub fn into_return_value(self) -> Option<CommandReturnValue> {
        match self {
            CommandItem::ReturnValue(rv) => Some(rv),
            _ => None,
        }
    }
}

impl<'a> Stream for CommandStream<'a> {
    type Item = crate::Result<CommandItem>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            let token = match ready!(this.token_stream.poll_next_unpin(cx)) {
                Some(res) => res?,
                None => return Poll::Ready(None),
            };

            return match token {
                ReceivedToken::NewResultset(meta) => {
                    let column_meta = meta
                        .columns
                        .iter()
                        .map(|x| Column {
                            name: x.col_name.to_string(),
                            column_type: ColumnType::from(&x.base.ty),
                        })
                        .collect::<Vec<_>>();

                    let column_meta = Arc::new(column_meta);
                    this.columns = Some(column_meta.clone());

                    this.result_set_index = this.result_set_index.map(|i| i + 1);

                    let query_item =
                        CommandItem::metadata(column_meta, *this.result_set_index.get_or_insert(0));

                    Poll::Ready(Some(Ok(query_item)))
                }
                ReceivedToken::Row(data) => {
                    let Some(columns) = this.columns.as_ref() else {
                        return Poll::Ready(Some(Err(crate::Error::Protocol(
                            "ROW token arrived before any column metadata".into(),
                        ))));
                    };
                    let columns = columns.clone();
                    let result_index = this.result_set_index.unwrap_or(0);

                    let row = Row {
                        columns,
                        data,
                        result_index,
                    };

                    Poll::Ready(Some(Ok(CommandItem::Row(row))))
                }
                ReceivedToken::ReturnStatus(rs) => {
                    Poll::Ready(Some(Ok(CommandItem::ReturnStatus(rs))))
                }
                ReceivedToken::ReturnValue(rv) => {
                    Poll::Ready(Some(Ok(CommandItem::ReturnValue(CommandReturnValue {
                        name: rv.param_name,
                        ord: rv.param_ordinal,
                        data: rv.value,
                    }))))
                }
                token => match token.records_affected() {
                    Some(rows) => Poll::Ready(Some(Ok(CommandItem::RowsAffected(rows)))),
                    None => continue,
                },
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tds::codec::{DoneStatus, TokenColMetaData, TokenDone, TokenRow};
    use enumflags2::BitFlags;
    use futures_util::stream;

    const INSERT: u16 = 0xC3;
    const UPDATE: u16 = 0xC5;
    const SELECT: u16 = 0xC1;

    fn meta() -> ReceivedToken {
        ReceivedToken::NewResultset(Arc::new(TokenColMetaData { columns: vec![] }))
    }

    fn row() -> ReceivedToken {
        ReceivedToken::Row(TokenRow::new())
    }

    fn count(cur_cmd: u16, rows: u64) -> TokenDone {
        TokenDone::new(DoneStatus::More | DoneStatus::Count, cur_cmd, rows)
    }

    async fn collect(tokens: Vec<ReceivedToken>) -> CommandResult {
        let tokens = stream::iter(tokens.into_iter().map(Ok)).boxed();
        CommandStream::new(tokens)
            .into_command_result()
            .await
            .unwrap()
    }

    fn set_sizes(res: &CommandResult) -> Vec<usize> {
        res.query_results.iter().map(Vec::len).collect()
    }

    #[tokio::test]
    async fn empty_result_sets_keep_their_index() {
        let res = collect(vec![
            meta(),
            row(),
            ReceivedToken::Done(count(SELECT, 1)),
            meta(),
            ReceivedToken::Done(count(SELECT, 0)),
            meta(),
            ReceivedToken::Done(count(SELECT, 0)),
            meta(),
            row(),
            row(),
            ReceivedToken::Done(count(SELECT, 2)),
        ])
        .await;
        assert_eq!(set_sizes(&res), [1, 0, 0, 2]);
    }

    #[tokio::test]
    async fn leading_empty_result_sets_are_kept() {
        let res = collect(vec![
            meta(),
            ReceivedToken::Done(count(SELECT, 0)),
            meta(),
            ReceivedToken::Done(count(SELECT, 0)),
            meta(),
            row(),
            ReceivedToken::Done(count(SELECT, 1)),
        ])
        .await;
        assert_eq!(set_sizes(&res), [0, 0, 1]);
    }

    #[tokio::test]
    async fn rows_affected_skips_selects_and_uncounted_dones() {
        let res = collect(vec![
            ReceivedToken::DoneInProc(count(INSERT, 2)),
            meta(),
            row(),
            ReceivedToken::DoneInProc(count(SELECT, 1)),
            // `SELECT @v = ..` inside the procedure
            ReceivedToken::DoneInProc(count(SELECT, 1)),
            ReceivedToken::DoneInProc(count(UPDATE, 0)),
            ReceivedToken::ReturnStatus(-1),
            ReceivedToken::DoneProc(TokenDone::new(BitFlags::empty(), 0xE0, 0)),
        ])
        .await;
        assert_eq!(res.rows_affected(), [2, 0]);
        assert_eq!(res.return_code(), -1);
        assert_eq!(set_sizes(&res), [1]);
    }
}
