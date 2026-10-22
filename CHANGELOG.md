# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This is a maintained community fork of [`tiberius`](https://github.com/prisma/tiberius),
published on crates.io under the package name
[`tiberius-ng`](https://crates.io/crates/tiberius-ng). The library name remains
`tiberius`, so `use tiberius::…` code is unaffected. Entries up to and including
version 0.12.3 reflect the history of the upstream project prior to the fork.

## [Unreleased]

**0.13.1 is a stable settling point** — the fork has cleared the
security advisories, worked through the upstream backlog, and hardened the
codebase (100% mutation score, ~93% line coverage against a live SQL Server in
CI). No further releases are planned in the immediate term; the crate is
considered stable for production use.

Bug reports and feature requests are welcome via
[GitHub Issues](https://github.com/MattJackson/tiberius-ng/issues); usage
questions and general discussion via
[Discussions](https://github.com/MattJackson/tiberius-ng/discussions). Fixes
that land will be documented here.

### Added

- **`CancelHandle` cancels a request while it is being awaited.**
  `Client::cancel_handle` returns one, for use from another task or a `select!`
  branch. Unlike `cancel_query`, it also stops a request the server hasn't
  answered yet, such as a statement still sorting, aggregating or waiting on a
  lock.
- **`Config::connect_timeout`** carries `Connect Timeout` / `Connection Timeout`
  / `Timeout` (ADO.NET, 0 = none, default 15 s) and `loginTimeout` /
  `connectTimeout` (JDBC); wrap TCP connect and `Client::connect` in it.

### Fixed

- **`cancel_query` no longer desyncs the connection.** When SQL Server sent the
  Attention acknowledgement in a message of its own, `cancel_query` returned an
  error and left the acknowledgement for the next query to read as its result.
  It now reads on to the acknowledgement, discarding the cancelled response,
  including any server error in it.
- **Nor does a `cancel_query` future dropped partway.** Dropped while waiting
  for the acknowledgement, as under a timeout, it left the acknowledgement for
  the next request to read as its result. The next request now reads up to it
  first, or, if the future was dropped partway through reading a token over 1
  MiB, fails along with every later one.
- **A response left partly unread keeps its transaction changes.** The rest of a
  dropped stream was discarded, including the ENVCHANGE of a `BEGIN TRAN` or
  `COMMIT` in it, so the server rejected the next request (3989) or every later
  one (3971). It is now read to the end. A token abandoned partway, its stream
  dropped while waiting for the next packet, is read again from its start unless
  it is over 1 MiB; then the rest of that message is still discarded.
- **A request that fails to encode no longer hangs the connection.** After an
  encode error, such as a TVP value too long for its column, the next request
  waited for a response to the request never sent.
- **Row counts match SqlClient's `ExecuteNonQuery`.**
  `ExecuteResult::rows_affected`/`total`, `CommandResult::rows_affected` and
  `CommandItem::RowsAffected` counted SELECTs and variable assignments (one
  INSERT came back as 4); they now count only statements whose DONE has
  DONE_COUNT and a non-SELECT CurCmd. *Behavior change*: fewer entries in
  `rows_affected()`.
- **`into_command_result` keeps empty result sets.** An empty result set after
  the first vanished and shifted later ones down an index.
- **Later server errors and info messages are no longer dropped.**
  `Error::Server` still carries the first error; `TokenError::other_errors` and
  `TokenError::info_messages` (new) return the rest of the response's ERROR and
  INFO tokens.
- **A DONE with DONE_ERROR and no ERROR token fails the request**, with
  SqlClient's class 11 "A severe error occurred" error, and DONE_SRVERROR adds a
  class 20 one; both were ignored. A cancelled request still ends without an
  error. *Behavior change*.
- **A LOGINACK below TDS 7.2 fails the connect** with `Error::Protocol`, as in
  SqlClient: requests carry ALL_HEADERS and tokens are read in their 7.2
  layouts. `docs/TDS_COMPATIBILITY.md` no longer claims TDS 7.1 support.
- **`DateTime<Utc>` from a `datetimeoffset` is no longer shifted by the
  offset.**
- **`try_get::<rust_decimal::Decimal>` returns a conversion error instead of
  panicking** on scale > 28 or more than 96 bits; trailing zeros are dropped
  first, as SqlClient does.
- **An unpaired UTF-16 surrogate decodes as U+FFFD instead of failing the result
  set**, in values (nvarchar, nchar, ntext, xml, sql_variant) and in
  column/table names, messages and ENVCHANGEs.
- **An invalid code-page sequence decodes as U+FFFD instead of failing the
  result set** (0x82 0x20 in a Japanese_CI_AS column), in char, varchar, text
  and sql_variant values. SqlClient substitutes `?`.
- **An ntext value of odd byte length is an error**, as an nvarchar one is,
  instead of losing its last byte.
- **A `sql_variant` holding a UTF-8-collation varchar decodes as UTF-8**, not as
  the locale's code page.
- **The 20 CP437/CP850 SQL collations (e.g. `SQL_Latin1_General_CP850_CI_AS`)
  can be read**, and bulk-load rows for them are encoded in their code page.
- **`Numeric::precision()` no longer counts a leading zero** (0.5 is 1, not 2),
  so a `decimal(38,38)` value is 38, not 39, and can be sent back as a
  parameter.
- **`datetime` is rounded to the millisecond, not truncated** (`.997` was read
  as `.996666…`), for both chrono and time, as SqlClient does.
- **A NULL `smalldatetime` decodes as `SmallDateTime(None)` in NBCROW rows too**
  (was `DateTime(None)`).
- **`Numeric` displays a scale-0 value without `.0`** (`123`, not `123.0`).
- **Malformed values return errors instead of panicking**: a smalldatetime of
  1440+ minutes or a datetime past midnight, a sql_variant or decimal magnitude
  ≥ 2^127, a decimal scale > 38, and `Numeric` equality across a wide scale gap.
- **A deserialized `Numeric` with a scale over 38 is a serde error** instead of
  a value that panics in `int_part`, `dec_part` and `Display`.
- **A precision-typed column other than decimal/numeric is a protocol error**
  instead of a `todo!()` panic; the decoder never builds one.
- **A `datetimeoffset` whose local time is past 9999-12-31 reads as a UTC
  `time::OffsetDateTime`** instead of panicking; only a malformed date is.
- **A PRELOGIN answer over 32768 bytes is an error** instead of a buffer that
  grows for as long as the server sends; SqlClient reads it from one packet.
- **FEDAUTHINFO offset + length can't overflow** on 32-bit targets.
- An empty `XmlData` parameter no longer corrupts the request (error 8009).
- An RPC parameter name over 255 UTF-16 code units is an error instead of a
  wrapped length byte (a debug-build panic).
- A TVP row with more fields than the type has columns is an error instead of a
  panic; a row with fewer fields is an error instead of a corrupted request.
- String parameters choose `nvarchar(4000)` or `nvarchar(max)` by UTF-16 length,
  not UTF-8 byte length, so 1334–4000 non-ASCII characters no longer get a
  second cached plan.
- `BigDecimal::to_sql`/`into_sql` no longer panic on values `decimal(38, s)`
  cannot hold exactly (`1/3`, `1e40`, `1e-39`); the query fails with
  `Error::Conversion` instead. Trailing zeros past scale 38 are dropped.
- chrono and `time` dates before year 1 no longer panic in `to_sql`/`into_sql`,
  nor does a `time::OffsetDateTime` whose UTC date passes 9999-12-31; dates
  outside 0001-01-01..9999-12-31 fail the query with `Error::Conversion`.
- money/smallmoney TVP and bulk values out of range (or NaN) are an
  `Error::Conversion` instead of silently saturating (smallmoney 300000 was
  stored as 214748.3647). A `Numeric` is converted exactly instead of through
  `f64`.
- OUT parameters bound as `None` work: NULL parameters are sent with their type
  instead of as an untyped NULL, which the server rejects for OUT parameters
  (8015). OUT strings and binary are sent as `nvarchar(max)`/`varbinary(max)`,
  so a `(max)` result is no longer cut to 4000 characters or 8000 bytes.
- **Logins over one packet work with `Encrypt=false`.** Only the first LOGIN7
  packet is now sent through TLS (MS-TDS 3.2.5.2); the server dropped long AAD
  tokens and SSPI blobs.
- **ADO.NET strings are parsed with SqlClient's rules:** `""`/`''` are one
  quote, non-ASCII values are accepted, whitespace (TAB, CR, LF) before a key is
  skipped instead of making it a different key, and `Encrypt` accepts
  `Mandatory`/`Optional`.
- **`TrustServerCertificate=true` with `TrustServerCertificateCA`** returns an
  error from `from_ado_string`/`from_jdbc_string` instead of panicking.
- **JDBC `serverName`/`server`, `portNumber`/`port` and `instanceName`
  properties** are honoured instead of falling back to `localhost:1433`.
- **`host\INSTANCE,port` connects to `port`,** as documented for mssql-jdbc: the
  instance name no longer goes to the server for validation, and `connect_named`
  skips the SQL Browser (which it used to query on the TCP port).
- **LOGIN7 strings over 128 characters** fail with an error naming the field
  instead of an I/O error once the server drops the connection.
- **Prelogin answers:** unknown options are skipped, a non-empty THREADID no
  longer panics debug builds, and a 1-byte INSTOPT is read correctly.
- **Encryption follows the MS-TDS 2.2.6.5 table**, except that `Off` against a
  server without encryption fails instead of sending the login unencrypted, as a
  man in the middle could otherwise get it by claiming no encryption.
- **The TLS handshake survives partial writes:** a flight interrupted by
  `Pending` lost its tail, and flights over 64 KB panicked; flights are now
  split into 4 KB PRELOGIN packets.
- **TDS 7.x TLS is capped at TLS 1.2 under `native-tls`/`vendored-openssl`**;
  TLS 1.3's post-handshake messages broke the connection.
- **Bulk/TVP integers and floats are written at the column's width** (`-5i32`
  into `bigint` was stored as 4294967291, `1.5f32` into `float` as 5.3e-315);
  values out of the column's range are a `Conversion` error.
- **Bulk insert into a char/varchar/text column with a non-default collation**
  declares that collation in `INSERT BULK`, as SqlBulkCopy does (`αβ` into a
  Greek column was stored as `aa`). Needs SQL Server 2012 or later; older
  servers keep the old behaviour.
- **A row that fails to encode no longer corrupts the bulk insert**; the request
  stays usable for further rows and `finalize`.
- **Dropping a `BulkLoadRequest` without `finalize` cancels the bulk insert**;
  the next request on the connection used to hang.
- **Bulk/TVP `(max)` values over 65,535 bytes** are accepted, up to 2^31 - 1
  bytes.
- **Bulk insert into `time(n)` and `datetimeoffset(n)` with n < 7** (error
  4875); values are truncated to the column's scale, as SqlClient does.
- **Bulk insert into `nvarchar(n)` with 2001 ≤ n ≤ 4000** (error 4816) and
  `nchar(n)` declared twice its length.
- **Bulk insert into `text`, `ntext` and `image` columns** (error 4804, `image`
  unsupported).
- **`sql_variant` strings in bulk/TVP** carry the database collation (error
  8001).
- **Bulk/TVP into `NOT NULL` `money`, `smallmoney`, `datetime` and
  `smalldatetime` columns**, and `datetime2` values into `smalldatetime`
  columns, which were written as `datetime`.
- **`INSERT BULK` column names are quoted with `]` doubled** (a column named
  `we]ird` broke the statement).
- **Bulk/TVP `datetime2` into `datetime` rounds to 1/300 s** as `CAST` does,
  instead of truncating, and accepts dates from 1753-01-01 rather than
  1900-01-01.
- **A `time` of 24 hours or more is a `Conversion` error.** A hand-built `Time`
  too large for its scale's 3 to 5 bytes panicked in `encode`; one that fit was
  sent, and the server rejected it (8023).
- **A chrono leap second is a `Conversion` error** instead of a time past
  23:59:59.9999999 (8023) or, at any other minute, the next second (12:34:60.5
  was stored as 12:35:00.5).
- **A chrono `FixedOffset` with seconds is a `Conversion` error** instead of
  being cut to whole minutes (+01:00:30 was stored as +01:00, its local time 30
  s off). An offset beyond ±14:00 now fails before it is sent (was 8023).
- **A bulk/TVP decimal its column cannot hold is a `Conversion` error for that
  row**, checked after rounding to the column's scale, as SqlBulkCopy does.
  999.995 into `decimal(5,2)` failed the whole insert (9803), and a value of 39
  digits panicked debug builds.
- **TVP schema and type names over 255 UTF-16 units and procedure names over
  65,535 are errors** instead of wrapped length prefixes (a debug-build panic).
- **Strings over 1 GiB of UTF-8 are declared `nvarchar(max)`**, not `ntext(max)`
  (error 2716); one longer than `nvarchar(max)` holds (1,073,741,823 UTF-16
  units) is a `Conversion` error.
- **NULL binary and decimal parameters are declared like values of their type**,
  `varbinary(8000)` and `numeric(38,0)` instead of `varbinary(max)` and
  `numeric` (18,0), so they no longer get a cached plan of their own.
- **Bulk/TVP `DateTime<Utc>` and `NaiveDateTime` go into `datetimeoffset`
  columns** (at +00:00) **and `date` columns**, as SqlBulkCopy converts them,
  and **bulk insert into `xml` columns works** (error 4816).
- **A TLS handshake over OpenSSL survives a socket that can't take a flight at
  once.** OpenSSL fails the handshake on a flush that would block, which
  affected `native-tls` on Linux and `vendored-openssl`.
- **ADO.NET strings take SqlClient's other keywords and synonyms:** `Addr`,
  `Address`, `Network Address`, `App`, `WSID`, `Trusted_Connection`, `Packet
  Size`, `connecttimeout` and the spaced spellings, such as `Application Intent`
  and `Trust Server Certificate`.
- **The last of an ADO.NET key's synonyms wins,** as in SqlClient:
  `Server=a;Data Source=b` connects to `b`, not `a`.
- **`Data Source` takes `TCP:` in any case and spaces around the port;** `np:`,
  `lpc:`, `admin:` and pipe paths are errors instead of host names.
- **`Integrated Security` without an authentication backend is an error** naming
  the feature (`winauth`; `integrated-auth-gssapi` or `sspi-rs`), instead of a
  SQL login as user "".
- **PRELOGIN's TRACEID is MS-TDS 2.2.6.5's 36 bytes** (connection id, activity
  id, sequence), not 20.
- **JDBC URLs are split as mssql-jdbc splits them:** a bare `\` and non-ASCII
  values parse, `}}` in braces is `}`, names and unbraced values are trimmed,
  and the last of a property's synonyms wins.

### Changed

- **Large requests are sent faster.** Their packets reach the socket in a few
  large writes instead of one write each, while every TLS record still holds
  whole packets, as SQL Server requires. Over TLS, sending a 2 MB parameter took
  about 11 ms and now takes about 3.3 ms.
- **Reading results is faster.** Packets are buffered without a copy per value,
  values are read in bulk rather than a byte at a time, and strings are decoded
  straight into a buffer of the right size. On recorded responses, 1M narrow
  rows take 24% less time, 200k wide rows read 3.4 times as fast, and a 50 MB
  nvarchar(max) value 21 times as fast.
- **`begin_transaction()` asks for READ COMMITTED** instead of
  `IsolationLevel::Unspecified`, which keeps the session's current level (MS-TDS
  2.2.6.9), so a transaction after a SERIALIZABLE one was SERIALIZABLE too. This
  matches SqlClient's `BeginTransaction()`. The `IsolationLevel` docs now say
  that a level outlives its transaction.
- **`ColumnType` reports `Money4` and `Datetime4` for nullable `smallmoney` and
  `smalldatetime`** (was `Money` and `Datetimen`).
- `Query::MAX_PARAMETERS` is 2098, not 2100: `sp_executesql` takes two of the
  server's 2100. Binding more is now a client-side `Error::Conversion` before
  anything is sent.
- Decimal parameters are declared `numeric(38, s)` with the value's scale,
  instead of a precision taken from the value, so values of different digit
  counts share a cached plan. A value of more than 38 digits is now a
  client-side `Error::Conversion`.
- `MetaDataColumn`'s `Display` output is a valid `INSERT BULK` column
  definition: n-type lengths count characters, `time`/`datetimeoffset` show
  their scale, nullable 4-byte `datetime` shows as `smalldatetime`, and `]` in
  names is doubled.

### Security

- **`Encrypt=Strict` ignores `TrustServerCertificate`,** as SqlClient does, so
  the server certificate is always validated (`TrustServerCertificateCA` still
  applies).
- **Secrets are zeroized before their memory is freed:** connection-string
  values, the AAD token (also when a connect is abandoned) and the LOGIN7
  buffer, which used to grow and free copies of the obfuscated password.
- **Parsing a JDBC URL frees no copy of its password;** the old parser grew its
  buffers a character at a time.

### Breaking

- **RETURN status is `i32`**: `CommandItem::ReturnStatus(i32)`,
  `CommandItem::as_return_status() -> Option<i32>`,
  `CommandResult::return_code() -> i32`. `RETURN -1` used to come back as
  4294967295.
- **`money`/`smallmoney` decode to an exact scale-4 `ColumnData::Numeric`
  instead of a lossy `F64`** (`922337203685477.5807` and `.5806` both read as
  `…477.6`); `f64`/`f32` `FromSql` now accept `Numeric`, so `row.get::<f64,
  _>()` keeps working.
- **Behaviour changes from the fixes above:** `Encrypt=Strict` with
  `TrustServerCertificate=true` now fails against an untrusted certificate;
  connection strings .NET rejects (an unquoted value ending in a quote, an
  unclosed quote) are errors; `key=;` unsets `key`; an instance name with an
  explicit port is ignored; `Encrypt=false` against a server that answers
  ENCRYPT_NOT_SUP fails before TLS rather than during it.
- **ADO.NET keys SqlClient doesn't know are errors** ("Keyword not supported"),
  as are keywords the crate can't honour without changing the connection:
  `MultipleActiveResultSets=true`, `Failover Partner`, `Column Encryption
  Setting=Enabled`, `AttachDbFilename`, `Current Language`, `Server
  Certificate`, `User Instance=true`, `Replication=true`, Entra ID
  `Authentication` values and a non-TCP `Network Library`. Pooling, retry,
  timeout and enlistment keywords are accepted and ignored. Invalid `Integrated
  Security` values are errors.
- **JDBC:** `{a}}b}` is `a}b`, not `a}b}`; a URL must start with
  `jdbc:sqlserver://` and its server name takes no braces; an `instanceName`
  property wins over `serverName=host\instance`; a `{` after the start of a
  value is an error, as in mssql-jdbc (`a{/}b` used to mean `a/b`).

## [0.13.1] - 2026-08-30

A follow-up "polish" patch on top of the 0.13.0 release.

### Changed

- **Connection-string encryption now defaults to `Required`.** When an
  ADO.NET/JDBC connection string omits the `encrypt` keyword, the negotiated
  encryption level is now `Required` (matching `Config::default()` and modern
  ADO.NET's `Encrypt=Mandatory`) instead of `Off`. Opt out explicitly with
  `encrypt=false` (or `encrypt=DANGER_PLAINTEXT`). *Behavior change* for
  connection strings that relied on the previous plaintext default.

### Fixed

- **Read-side connection safety.** If a result stream is abandoned partway
  through (e.g. a cancelled `query`/`execute` future), `flush_stream` now
  poisons the connection when it cannot drain cleanly, so the connection fails
  fast on next use instead of being silently reused in an inconsistent state
  (complementing the send-side poisoning added in 0.13.0).

### Developer experience

- **`#[derive(TableValueRow)]`** now reports unsupported inputs (enums, unions,
  tuple/unit structs, multiple lifetimes) with a proper `compile_error!` at the
  offending span instead of panicking, and documents the supported column field
  types.
- **Mutation testing.** Added a sharded [cargo-mutants](https://mutants.rs)
  harness (`.cargo/mutants.toml`, `.github/workflows/mutants.yml`, and
  `scripts/mutants-ec2.sh` for on-demand EC2 fan-out) and closed the surviving
  mutants it found across the codec, config, numeric, result and row layers with
  new unit tests. Wire-serialization encoders, platform-specific FFI, and
  feature/`cfg`-divergent code that cannot be exercised by the offline unit
  suite are documented exclusions; the gate fails on any surviving mutant in the
  remaining logic. Several equivalent mutants were also removed by minor
  simplifications (e.g. `1 << 0` → `1`). The sweep reaches a **100% mutation
  score** (0 survivors) over the covered logic.
- **Coverage.** CI now measures line coverage with
  [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) against a live
  SQL Server 2022 container and publishes to Codecov (~93% lines). Added offline
  codec unit tests and a `transactions` integration suite (commit/rollback,
  explicit isolation levels) that piggybacks the existing CI SQL Server.
- **Lint hygiene.** `cargo clippy --features all --all-targets` is clean under
  `-D warnings`, including the test and example targets.

## [0.13.0] - 2026-08-29

First release of the actively-maintained community fork, by
[@MattJackson](https://github.com/MattJackson). Published on crates.io as
[`tiberius-ng`](https://crates.io/crates/tiberius-ng); the importable library
name is still `tiberius`, so downstream `use tiberius::…` code is unaffected —
only the dependency line changes:

```toml
tiberius = { package = "tiberius-ng", version = "0.13" }
```

This release clears the security advisories that were blocking adoption, works
through a large backlog of community pull requests (attributed below), fixes
long-standing bug reports, and substantially extends TDS protocol coverage
(TDS 7.1 through 8.0). See [`docs/TDS_COMPATIBILITY.md`](docs/TDS_COMPATIBILITY.md).

### Security

- Upgraded the TLS stack (`tokio-rustls` 0.24 → 0.26, `rustls` 0.21 → 0.23),
  clearing **RUSTSEC-2026-0098/-0099/-0104** and unblocking `cargo audit` /
  `cargo deny`. From upstream #419 by [@jakewimmer](https://github.com/jakewimmer).
  Fixes #417, #428.
- Zeroize SQL authentication password buffers before/after the login exchange.
  From #411 by [@lstkz](https://github.com/lstkz).
- Added a `cargo-deny` supply-chain gate (`deny.toml` + Security-audit workflow)
  and a Codecov coverage workflow; enforced in CI on every push/PR.
- **Untrusted-input hardening (multi-round code audit).** Malformed or hostile
  server responses can no longer panic, overflow, hang or exhaust memory in the
  decoders: bounded the "unknown length" PLP accumulator and SESSIONSTATE /
  ALTMETADATA / ORDER allocations; rejected out-of-range `decimal`/`numeric`
  magnitudes, `datetime`/`datetimeoffset` values and negotiated packet sizes;
  guarded the SQL-Browser (SSRP) reply parser; and made the `time`/`chrono`
  conversions and the rustls native-root loader return errors instead of
  panicking. A cancelled multi-packet write now poisons the connection so it
  fails cleanly rather than desyncing the wire.

### Added — connectivity & protocol

- **TDS 8.0 "strict" encryption** (`EncryptionLevel::Strict`, TLS-before-prelogin
  with the `tds/8.0` ALPN), plus `Config::hostname_in_certificate()` and
  `Config::client_name()`. Adapted from #413 by [@olback](https://github.com/olback).
  Fixes #412, #340, #414, #224.
- **SQL_VARIANT** reading (previously panicked, `todo!()`) and writing — full
  `sql_variant` parameter support, symmetric with the decoder.
- **CLR UDT**, **COLINFO**, **TABNAME**, **FEDAUTHINFO**, **SESSIONSTATE**, and
  **ALTMETADATA/ALTROW** (`COMPUTE BY`) token support.
- **Transaction Manager requests** (begin/commit/rollback) and the **Attention**
  signal for query cancellation.
- **`Client::column_metadata()`** exposing column type/size/precision/scale,
  nullability and identity flags. From #398 by [@etylermoss](https://github.com/etylermoss).
  Fixes #397, #217, #403.
- Emit PRELOGIN INSTOPT/TRACEID options.

### Added — API & types

- Optional **`serde`** feature: `Serialize`/`Deserialize` for the result types.
  From #416 by [@MukundaKatta](https://github.com/MukundaKatta). Fixes #115.
- **`ConfigBuilder`** for ergonomic `Config` construction. From #366 by
  [@LonerDan](https://github.com/LonerDan).
- **IN-list / 2100-parameter helpers.** From #429 by
  [@joelparkerhenderson](https://github.com/joelparkerhenderson). Fixes #157.
- `bulk_insert_columns()` to bulk-insert into a specified column list. From #359
  by [@NTmatter](https://github.com/NTmatter). Fixes #311.
- `packet_size` configuration for the LOGIN7 message. From #400 by
  [@johndauphine](https://github.com/johndauphine).
- `IntoSql` for `rust_decimal::Decimal` (#376 by [@esheppa](https://github.com/esheppa),
  fixes #401); more `ColumnData` conversion traits (#314) and `Row`→`ColumnData`
  / `TokenRow` accessors (#304, #331) by [@LazyDope](https://github.com/LazyDope)
  and [@lpj145](https://github.com/lpj145).
- `docker/test-server.sh` helper and a named-pipes connection example (#430, #132).

### Fixed

- Return an `Error` instead of **panicking** on unexpected server input across
  the TDS decoder, and when the server declines the requested encryption level.
  Fixes #424, #425.
- Corrected several protocol-conformance edges: the `date` TYPE_INFO no longer
  emits a spurious scale byte (which desynced bulk-load/TVP metadata after a
  `date` column); `ERROR`/`INFO` `LineNumber` is read with the correct
  TDS-7.2-versioned width; a typeless `NULL` column decodes consistently across
  the ROW and NBCROW paths; and a big-endian word-swap that corrupted large
  decimals on BE hosts was removed.
- Bounds-check column indexing so out-of-range `try_get` returns `Err`, not a
  panic (#211); match raw-identifier column names like `r#type` (#382).
- Fix a multiply-overflow panic decoding dates before 1900 (#316).
- Error at connect time when encryption is required but no TLS backend is
  compiled in (#305).
- Correct the swapped old/new values in `EnvChange` `Display` (#418); lower
  chatty per-connection/token logs from `info!` to `debug!` (#281).
- Convert SQL `smallint`/`Intn` into `i32` via `FromSql` (#263); coerce
  `DateTime2` → `datetime` for bulk insert under `tds73` (#298, from
  [@Geo-W](https://github.com/Geo-W); fixes #307, #373); coerce numerics into
  Money/SmallMoney and strings into NText/Text during bulk insert (#358, #352).
- Send the ReadOnly intent flag in LOGIN7 when `ApplicationIntent=ReadOnly` (#348).
- Fix the sign/padding of negative `Numeric` string formatting. From #390 by
  [@zuckschwerdt](https://github.com/zuckschwerdt). Fixes #368.
- Rescale `numeric`/`decimal` parameters to the target column scale (overflow-
  checked scale-up, round-half-away-from-zero scale-down) instead of panicking
  on a scale mismatch.
- Allow querying columns whose names are keywords such as `End` (#388, from
  [@cjordan](https://github.com/cjordan)).
- Improve `QueryStream::into_results` handling of empty results (#385, fixes #380);
  fix a header type for the SSPI response message (#351, from
  [@staticlibs](https://github.com/staticlibs)); correct an `occured` typo in an
  I/O error message (#423, from [@DucMinhNe](https://github.com/DucMinhNe)).

### Changed — tooling & housekeeping

- Rebranded to the maintained fork (`tiberius-ng` 0.13.0); repository, docs.rs,
  badges and metadata now point at
  [MattJackson/tiberius-ng](https://github.com/MattJackson/tiberius-ng).
- Modernized the GitHub Actions workflows (SHA-pinned actions); replaced the
  broken upstream security workflow; added Dependabot, issue/PR templates,
  `CODE_OF_CONDUCT.md`, `CONTRIBUTING.md`, and `SECURITY.md`.
- Restructured CI around the `dev → qa → main` lifecycle: a fast lane on
  PRs/`dev` (lint, unit tests, one integration smoke) and the full UAT matrix on
  `qa` — SQL Server 2017/2019/2022/**2025** and Azure SQL Edge across every
  feature combination, plus Windows and macOS. Added a tag-triggered release
  workflow (verify → publish to crates.io → GitHub release) and Codecov coverage.
- Fixed the docs.rs build (use the `docsrs` cfg instead of a nightly-only `docs`
  feature) and cleared all `clippy -D warnings`.
- Modernized dependencies and added ~90 unit tests, roughly doubling coverage.
- Declared an MSRV of Rust 1.88 (`rust-version`) with a dedicated CI job, and
  added an advisory `cargo-semver-checks` job plus a `cargo-deny` license gate.

### Removed

- **Dropped async-std support.** The discontinued `async-std` runtime
  (RUSTSEC-2025-0052) is no longer a supported runtime: the
  `sql-browser-async-std` feature, its SQL Browser implementation, and the
  async-std example/tests are removed, eliminating `async-std` from the
  dependency graph entirely. Tiberius remains runtime-independent — use **tokio**
  or **smol** (or any `AsyncRead + AsyncWrite` transport). The internal
  dual-runtime test harness now exercises every integration test on tokio **and**
  smol. *Breaking* for downstreams using the `sql-browser-async-std` feature;
  switch to `sql-browser-tokio` or `sql-browser-smol`.

### Notes

- `0.13.0-alpha.1` was published to crates.io to claim the `tiberius-ng`
  package name ahead of this release.
- Deferred to a follow-up: client-certificate (mutual-TLS) authentication
  (upstream #413's client-cert portion), which needs a rebase onto the new TLS
  stack and live-server validation.

## [0.12.3]

- feat: improve column type accuracy (#347)
- fix: encoding of zero-length values for large varlen columns (#315)
- update tokio_rustls (#306)
- Allow iterating over the cells in a row. (#303)
- Send ReadOnlyIntent when ApplicationIntent=ReadOnly specified (#297)
- Replace encoding with encoding_rs (#285)
- Disable chrono's oldtime feature (#284)

## [0.12.2]

- Update connection-string crate to 0.2 (#286)

## [0.12.1]

- fix: bigdecimal conversion overflow (#271)
- Reduce futures crate dependency footprint (#270)

## [0.12.0]

- BREAKING: Correctly convert DateTimeOffset to/from database (#269)
  Please read the [issue](https://github.com/prisma/tiberius/issues/260)
  carefully before upgrading.

## [0.11.8]

- feat: improve column type info (#347)

## [0.11.7]

- chore: Update connection string to 0.2 (#286)

## [0.11.6]

- fix: bigdecimal conversion overflow (#271)

## [0.11.5]

- Close connection explicitly (#268)

## [0.11.4]

- Fix buffer overrun on finalize (#266)
- Correctly parse (local) server name (#259)

## [0.11.3]

- Cleanup TokenRow public API (#255)
- Fix null values in NBC rows (#253)

## [0.11.2]

- Fix error ordering (#248)

## [0.11.1]

- Don't load native roots for trust-all config (#243)
- Propagate errors correctly (#247)

## [0.11.0]

- BREAKING: bigdecimal crate upgraded to 0.3 major and has to be of
  the same major in other crates using Tiberius.
- Handle negative scale from a BigDecimal (#240)

## [0.10.0]

- BREAKING: uuid crate upgraded to 1.0 major and has to be of the same
  major in other crates using Tiberius.

## [0.9.5]

- Add fractional seconds precision for datetime2 (#235)

## [0.9.4]

- Fix SQL Browser response parsing error (#229)
- Bulk uploads (#227)

## [0.9.3]

- Enable SSL if using vendored-openssl feature (#225)

## [0.9.2]

- Allow statically linking against OpenSSL (#222)

## [0.9.1]

- Support AAD token authentication (#215)

## [0.9.0]

- (BREAKING) support rustls, switch between native-tls and rustls.
  the feature flag vendored-openssl is gone. instead if needing vendored TLS,
  use feature flag rustls

## [0.8.0]

- (BREAKING) fix: correctly decode null integers (#209)

## [0.7.3]

- Fixing an accidentally renamed time module, that would've been a breaking change.

## [0.7.2]

- Dynamic query interface (#196)
- Support for `time` 0.3.x (#201)
- Additional option to add custom-ca to root certificates (#203, thx @lostiniceland)

## [0.7.1]

- Support all pre-login tokens

## [0.7.0]

- Remove async-std from deps if using tokio
- show TokioAsyncWriteCompatExt in Client docs (#183)
- Upgrade to Rust edition 2021 (#180)

## [0.6.5]

- Constrain UUID features and optionalize winauth dependency (smaller binaries)

## [0.6.4]

- Use bundled bigint from bigdecimal

## [0.6.3]

- Bignum/bigint compilation problems fixed.

## [0.6.2]

- Improvement on waker calls. We used to wake the runtime too often, this should improve performance.

## [0.6.1]

- SQL Browser for the smol runtime.

## [0.6.0]

- Refactor stream handling to something more rusty (#166). This is a breaking
  change, if relying on the asynchronous stream handling of QueryResult. Please
  refer to the updated documentation.

## [0.5.16]

- Allow setting application name per connection (#161)

## [0.5.15]

- Split column decoding into modules (speeding up TEXT/NTEXT/IMAGE decoding a lot) (#153)

## [0.5.14]

- Handle collations for CHAR and TEXT values (#153)

## [0.5.13]

- Add Config parsing for "Integrated Security" (two words)
- Unified bitflag setup
- Correct default ports
- Update to enumflags2 0.7

## [0.5.12]

- Warnings should not affect metadata fetching (#139)

## [0.5.11]

- Fixing of all clippy warnings. This might have some performance benefits and
  might also fix some weird bugs in environments where we can't guarantee the
  evaluation order. (#136)
- Add info of LCID and sort id to colation errors (#138)

## [0.5.10]

- Remove a rogue `dbg!`

## [0.5.9]

- Set the `app_name` in LOGIN7 to `tiberius`. This allows connecting to servers
  that expect the value to not be empty (see issue #127).

## [0.5.8]

- Try out all resolved IP addresses (#124)

## [0.5.7]

- Set server name in the login packet (#122)

## [0.5.6]

- Fix for handling nullable values (#119 #121)

## [0.5.5] and [0.4.21]

Catastropichal build failures with feature flags fixed.

## [0.5.4] and [0.4.20]

Removed the tls feature flag to simplify dependencies. This means you will
always get a TLS-enabled build, and can disable it on runtime. This also means
we don't always compile async-std if wanting to use tokio, and so forth.

Fixes certain issues with vendored OpenSSL on macOS platforms too.

## [0.5.3]

Changed futures-codec2 to asynchronous-codec, due to former was yanked.

## [0.5.2] and [0.4.19]

Introducing working TLS support on macOS platforms.

Please read the issue:

https://github.com/prisma/tiberius/issues/65

## [0.5.1]

Internally upgrade bytes to 1.0. Should have no visible change to the apis.

## [0.5.0]

If using Tiberius with Tokio and SQL Browser, this PR will upgrade Tokio to 1.0.

0.4 branch will be updated for a short while if needed and until the ecosystem
has completely settled on Tokio 1.0.

## [0.4.18]

- Allow `databaseName` in connection string to define the database (#108)
- Implement reader functions for standard string data (#107)
- Fix a time conversion error (#106)

## [0.4.17]

- Fixing error swallowing with `simple_query` and MARS (#105)
- Fixing transaction descriptor reading (#105)
- Fixing envchange token reads (#105)

## [0.4.16]

- Handle all MARS results properly (#102)

## [0.4.14]

- Support alternatively `BigNumber` when dealing with numeric values.
- Document feature flags

## [0.4.13]

- Realizing UTF-16 works just fine with SQL Server. Reverting the UCS2, but
  still keeping the faster writes.

## [0.4.12]

*SKIP this, go directly to 0.4.13*

- A typo fix in README (#94)
- Faster string writes with better length handling. UCS2 for writes (#95).

## [0.4.11]

- Allow disabling TLS in connection string (#89)
- Use connection-string for ado.net parsing (#91)
- Handle JDBC connection strings (#92)

## [0.4.10]

- Handling nullable int values, fix for #78 (#80)
- Reflect tweaks to upstream libgssapi crate (#81)
- Skip default features in libgssapi (for macOS support)
- Handle env change Routing request (#87)

## [0.4.9]

- BREAKING: `AuthMethod::WindowsIntegrated` renamed to `AuthMethod::Integrated`.
- Use GSSAPI for IntegratedSecurity on Unix platforms
- Fix module docs for examples
- Make `packet_id` wrapping explicit
- Add DNS feature to Tokio

## [0.4.8]

- BREAKING: `ColumnData::I8(i8)` is now `ColumnData::U8(u8)` due to misunderstanding how `tinyint` works. (#71)
- Skip any received `done_rows` amounts and avoid creating extra resultsets (#67)
- Actually run the chrono tests (#72)
- Fix GUID byte ordering (#69)
- Fix null time/datetime2/datetimeoffset handling (#73)
- Null image data should be `Binary`, not `String`

## [0.4.7]

- Pass hostname to TLS handshake, allowing usage with AzureSQL using
  `TrustServerCertificate=no`
  ([#62](https://github.com/prisma/tiberius/pull/62))

## [0.4.5]

- Documenting type conversions and re-exporting chrono types
  ([#60](https://github.com/prisma/tiberius/pull/60))

## [0.4.4]

- Fixing multi-part table names in `IMAGE`, `TEXT` and `NTEXT` column metadata
  ([#58](https://github.com/prisma/tiberius/pull/58))

## [0.4.3]

- Starting transactions with `simple_query` now works without crashing
  ([#55](https://github.com/prisma/tiberius/pull/55))

## [0.4.2]

- Fixing old and wrong `ExecuteResult` docs
- Adding `rows_affected` method to `ExecuteResult`

## [0.4.1]

- Add all feature flags for docs.rs build

## [0.4.0]

- A complete rewrite from 0.3.0
- Not bound to Tokio anymore, independent of the runtime
- Support for many more types
- Async/await, futures 0.3

[Unreleased]: https://github.com/MattJackson/tiberius-ng/compare/v0.13.1...HEAD
[0.13.1]: https://github.com/MattJackson/tiberius-ng/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/MattJackson/tiberius-ng/compare/v0.12.3...v0.13.0
