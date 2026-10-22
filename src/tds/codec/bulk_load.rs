use asynchronous_codec::BytesMut;
use futures_util::io::{AsyncRead, AsyncWrite};
use tracing::{event, Level};

use crate::{
    client::Connection, sql_read_bytes::SqlReadBytes, BytesMutWithDataColumns, ExecuteResult,
};

use super::{
    Encode, MetaDataColumn, PacketHeader, PacketStatus, TokenColMetaData, TokenDone, TokenRow,
    HEADER_BYTES,
};

/// A handler for a bulk insert data flow.
#[derive(Debug)]
pub struct BulkLoadRequest<'a, S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    connection: &'a mut Connection<S>,
    packet_id: u8,
    buf: BytesMut,
    columns: Vec<MetaDataColumn<'a>>,
}

impl<'a, S> BulkLoadRequest<'a, S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    pub(crate) fn new(
        connection: &'a mut Connection<S>,
        mut columns: Vec<MetaDataColumn<'a>>,
    ) -> crate::Result<Self> {
        // The server now waits for the bulk-load message; until `finalize`
        // ends it, the next request on the connection has to abandon it.
        connection.set_bulk_load_open(true);
        let packet_id = connection.context_mut().next_packet_id();

        let collation = connection.context().collation();
        for column in columns.iter_mut() {
            column.base.set_variant_collation(collation);
        }
        let mut buf = BytesMut::new();

        let cmd = TokenColMetaData {
            columns: columns.clone(),
        };

        cmd.encode(&mut buf)?;

        let this = Self {
            connection,
            packet_id,
            buf,
            columns,
        };

        Ok(this)
    }

    /// Adds a new row to the bulk insert, flushing only when having a full packet of data.
    ///
    /// A row that fails to encode is not sent, and the request stays usable.
    /// Dropping the request without [`finalize`] cancels the bulk insert;
    /// the connection stays usable.
    ///
    /// # Warning
    ///
    /// After the last row, [`finalize`] must be called to flush the buffered
    /// data and for the data to actually be available in the table.
    ///
    /// [`finalize`]: #method.finalize
    pub async fn send(&mut self, row: TokenRow<'a>) -> crate::Result<()> {
        let row_start = self.buf.len();
        let mut buf_with_columns = BytesMutWithDataColumns::new(&mut self.buf, &self.columns);

        if let Err(e) = row.encode(&mut buf_with_columns) {
            self.buf.truncate(row_start);
            return Err(e);
        }
        self.write_packets().await?;

        Ok(())
    }

    /// Ends the bulk load, flushing all pending data to the wire.
    ///
    /// This method must be called after sending all the data to flush all
    /// pending data and to get the server actually to store the rows to the
    /// table.
    pub async fn finalize(mut self) -> crate::Result<ExecuteResult> {
        TokenDone::default().encode(&mut self.buf)?;
        self.write_packets().await?;

        let mut header = PacketHeader::bulk_load(self.packet_id);
        header.set_status(PacketStatus::EndOfMessage);

        let data = self.buf.split();

        event!(
            Level::TRACE,
            "Finalizing a bulk insert ({} bytes)",
            data.len() + HEADER_BYTES,
        );

        self.connection.write_to_wire(header, data).await?;
        self.connection.set_bulk_load_open(false);
        self.connection.flush_sink().await?;

        ExecuteResult::new(self.connection).await
    }

    async fn write_packets(&mut self) -> crate::Result<()> {
        let packet_size = (self.connection.context().packet_size() as usize) - HEADER_BYTES;

        while self.buf.len() > packet_size {
            let header = PacketHeader::bulk_load(self.packet_id);
            let data = self.buf.split_to(packet_size);

            event!(
                Level::TRACE,
                "Bulk insert packet ({} bytes)",
                data.len() + HEADER_BYTES,
            );

            self.connection.write_to_wire(header, data).await?;
        }

        Ok(())
    }
}

/// Asks for the collation names of the columns `SELECT TOP 0 {columns} FROM
/// {table}` returns, in order (SQL Server 2012 and later).
pub(crate) fn collation_names_query(select: &str) -> String {
    format!(
        "SELECT collation_name FROM sys.dm_exec_describe_first_result_set(N'{}', NULL, 0) ORDER BY column_ordinal",
        select.replace('\'', "''")
    )
}

/// One INSERT BULK column definition. As in SqlBulkCopy, it names the
/// column's collation: without one the server reads non-Unicode bytes in the
/// database default code page.
pub(crate) fn insert_bulk_column(column: &MetaDataColumn<'_>, collation: Option<&str>) -> String {
    match collation {
        Some(collation) => format!("{column} COLLATE {collation}"),
        None => column.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tds::codec::{BaseMetaDataColumn, ColumnFlag, FixedLenType, TypeInfo};
    use crate::tds::Collation;
    use crate::{VarLenContext, VarLenType};
    use std::borrow::Cow;

    fn column(name: &'static str, ty: TypeInfo) -> MetaDataColumn<'static> {
        MetaDataColumn {
            base: BaseMetaDataColumn {
                flags: ColumnFlag::Nullable.into(),
                ty,
            },
            col_name: Cow::Borrowed(name),
        }
    }

    #[test]
    fn insert_bulk_column_names_the_collation() {
        let a = column(
            "a",
            TypeInfo::VarLenSized(VarLenContext::new(
                VarLenType::BigVarChar,
                10,
                Some(Collation::new(0x00d0_0408, 0)),
            )),
        );
        let b = column("b]", TypeInfo::FixedLen(FixedLenType::Int4));

        assert_eq!(
            insert_bulk_column(&a, Some("Greek_CI_AS")),
            "[a] varchar(10) COLLATE Greek_CI_AS"
        );
        assert_eq!(insert_bulk_column(&b, None), "[b]]] int");
    }

    #[test]
    fn collation_names_query_quotes_the_select() {
        assert_eq!(
            collation_names_query("SELECT TOP 0 * FROM [o'brien]"),
            "SELECT collation_name FROM sys.dm_exec_describe_first_result_set(N'SELECT TOP 0 * FROM [o''brien]', NULL, 0) ORDER BY column_ordinal"
        );
    }
}
