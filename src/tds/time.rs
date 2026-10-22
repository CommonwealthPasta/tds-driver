//! Date and time handling.
//!
//! When using the `tds73` feature flag together with SQL Server 2008 or later,
//! the following [`time`] mappings to and from the database are available:
//!
//! - `Time` -> [`Time`](time/struct.Time.html)
//! - `Date` -> [`Date`]
//! - `DateTime` -> [`PrimitiveDateTime`]
//! - `DateTime2` -> [`PrimitiveDateTime`]
//! - `SmallDateTime` -> [`PrimitiveDateTime`]
//! - `DateTimeOffset` -> [`OffsetDateTime`]
//!
//! With SQL Server 2005 and the `tds73` feature flag disabled, the mapping is
//! different:
//!
//! - `DateTime` -> [`PrimitiveDateTime`]
//! - `SmallDateTime` -> [`PrimitiveDateTime`]
//!
//! [`time`]: time/index.html
//! [`Date`]: time/struct.Date.html
//! [`PrimitiveDateTime`]: time/struct.PrimitiveDateTime.html
//! [`OffsetDateTime`]: time/struct.OffsetDateTime.html

#[cfg(feature = "chrono")]
#[cfg_attr(docsrs, doc(cfg(feature = "chrono")))]
pub mod chrono;

#[cfg(feature = "time")]
#[cfg_attr(docsrs, doc(cfg(feature = "time")))]
// Submodule intentionally shares the name of the `time` feature/crate it wraps.
#[allow(clippy::module_inception)]
pub mod time;

use crate::{tds::codec::Encode, SqlReadBytes};
#[cfg(feature = "tds73")]
use byteorder::{ByteOrder, LittleEndian};
use bytes::{BufMut, BytesMut};
#[cfg(feature = "tds73")]
use futures_util::io::AsyncReadExt;

/// A presentation of `datetime` type in the server.
///
/// # Warning
///
/// It isn't recommended to use this type directly. For dealing with `datetime`,
/// use the `time` feature of this crate and its `PrimitiveDateTime` type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DateTime {
    days: i32,
    seconds_fragments: u32,
}

impl DateTime {
    /// Construct a new `DateTime` instance.
    pub fn new(days: i32, seconds_fragments: u32) -> Self {
        Self {
            days,
            seconds_fragments,
        }
    }

    /// Days since 1st of January, 1900 (including the negative range until 1st
    /// of January, 1753).
    pub fn days(self) -> i32 {
        self.days
    }

    /// 1/300 of a second, so a value of 300 equals 1 second (since midnight).
    pub fn seconds_fragments(self) -> u32 {
        self.seconds_fragments
    }

    pub(crate) async fn decode<R>(src: &mut R) -> crate::Result<Self>
    where
        R: SqlReadBytes + Unpin,
    {
        let days = src.read_i32_le().await?;
        let seconds_fragments = src.read_u32_le().await?;
        let dt = Self {
            days,
            seconds_fragments,
        };

        dt.millis_of_day()
            .map_err(|_| crate::Error::Protocol(format!("invalid datetime {dt:?}").into()))?;
        Ok(dt)
    }

    /// Milliseconds since midnight, rounded half up as SqlClient's
    /// `SqlDateTime` does (so `.997` stays `.997`, not `.996666…`).
    pub(crate) fn millis_of_day(self) -> crate::Result<u32> {
        const TICKS_PER_DAY: u32 = 300 * 86_400;

        if self.seconds_fragments >= TICKS_PER_DAY {
            return Err(crate::Error::Conversion(
                format!(
                    "datetime: {} ticks is past midnight",
                    self.seconds_fragments
                )
                .into(),
            ));
        }

        // ticks / 0.3: a third is never a tie.
        Ok((self.seconds_fragments * 10 + 1) / 3)
    }
}

impl Encode<BytesMut> for DateTime {
    fn encode(self, dst: &mut BytesMut) -> crate::Result<()> {
        dst.put_i32_le(self.days);
        dst.put_u32_le(self.seconds_fragments);

        Ok(())
    }
}

/// A presentation of `smalldatetime` type in the server.
///
/// # Warning
///
/// It isn't recommended to use this type directly. For dealing with
/// `smalldatetime`, use the `time` feature of this crate and its
/// `PrimitiveDateTime` type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SmallDateTime {
    days: u16,
    seconds_fragments: u16,
}

impl SmallDateTime {
    /// Construct a new `SmallDateTime` instance.
    pub fn new(days: u16, seconds_fragments: u16) -> Self {
        Self {
            days,
            seconds_fragments,
        }
    }
    /// Days since 1st of January, 1900.
    pub fn days(self) -> u16 {
        self.days
    }

    /// 1/300 of a second, so a value of 300 equals 1 second (since midnight)
    pub fn seconds_fragments(self) -> u16 {
        self.seconds_fragments
    }

    pub(crate) async fn decode<R>(src: &mut R) -> crate::Result<Self>
    where
        R: SqlReadBytes + Unpin,
    {
        let days = src.read_u16_le().await?;
        let seconds_fragments = src.read_u16_le().await?;
        let dt = Self {
            days,
            seconds_fragments,
        };

        dt.minutes_of_day()
            .map_err(|_| crate::Error::Protocol(format!("invalid smalldatetime {dt:?}").into()))?;
        Ok(dt)
    }

    /// Minutes since midnight; the wire field (`seconds_fragments`) holds minutes.
    pub(crate) fn minutes_of_day(self) -> crate::Result<u32> {
        match self.seconds_fragments {
            m @ 0..=1439 => Ok(m as u32),
            m => Err(crate::Error::Conversion(
                format!("smalldatetime: {m} minutes is past midnight").into(),
            )),
        }
    }
}

impl Encode<BytesMut> for SmallDateTime {
    fn encode(self, dst: &mut BytesMut) -> crate::Result<()> {
        dst.put_u16_le(self.days);
        dst.put_u16_le(self.seconds_fragments);

        Ok(())
    }
}

/// A presentation of `date` type in the server.
///
/// # Warning
///
/// It isn't recommended to use this type directly. If you want to deal with
/// `date`, use the `time` feature of this crate and its `Date` type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
pub struct Date(u32);

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl Date {
    #[inline]
    /// Construct a new `Date`
    ///
    /// # Panics
    /// max value of 3 bytes (`u32::max_value() > 8`)
    pub fn new(days: u32) -> Date {
        assert_eq!(days >> 24, 0);
        Date(days)
    }

    #[inline]
    /// The number of days from 1st of January, year 1.
    pub fn days(self) -> u32 {
        self.0
    }

    /// For `ToSql`, which cannot fail: a day count before year 1 becomes
    /// `u32::MAX`, which the encoder rejects like any day past 9999-12-31.
    #[cfg_attr(not(any(feature = "chrono", feature = "time")), allow(dead_code))]
    pub(crate) fn from_signed_days(days: i64) -> Date {
        Date(u32::try_from(days).unwrap_or(u32::MAX))
    }

    pub(crate) async fn decode<R>(src: &mut R) -> crate::Result<Self>
    where
        R: SqlReadBytes + Unpin,
    {
        let mut bytes = [0u8; 4];
        src.read_exact(&mut bytes[..3]).await?;
        Ok(Self::new(LittleEndian::read_u32(&bytes)))
    }
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl Encode<BytesMut> for Date {
    fn encode(self, dst: &mut BytesMut) -> crate::Result<()> {
        // 9999-12-31. The server rejects later dates, including a
        // datetimeoffset whose UTC date is later.
        const MAX_DAYS: u32 = 3_652_058;

        if self.days() > MAX_DAYS {
            return Err(crate::Error::Conversion(
                "date out of range: SQL Server dates run from 0001-01-01 to 9999-12-31".into(),
            ));
        }

        let mut tmp = [0u8; 4];
        LittleEndian::write_u32(&mut tmp, self.days());
        dst.extend_from_slice(&tmp[0..3]);

        Ok(())
    }
}

/// A presentation of `time` type in the server.
///
/// # Warning
///
/// It isn't recommended to use this type directly. If you want to deal with
/// `time`, use the `time` feature of this crate and its `Time` type.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
pub struct Time {
    increments: u64,
    scale: u8,
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl PartialEq for Time {
    fn eq(&self, t: &Time) -> bool {
        self.increments as f64 / 10f64.powi(self.scale as i32)
            == t.increments as f64 / 10f64.powi(t.scale as i32)
    }
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl Time {
    /// What a chrono leap second converts to: `ToSql` cannot fail, so the
    /// encoder rejects it.
    pub(crate) const LEAP_SECOND: u64 = u64::MAX;

    /// Construct a new `Time`
    pub fn new(increments: u64, scale: u8) -> Self {
        Self { increments, scale }
    }

    /// Fails unless the server holds the value: under 24 hours, scale 0 to 7.
    pub(crate) fn validate(self) -> crate::Result<()> {
        self.len()?;

        if self.increments < 86_400 * 10u64.pow(u32::from(self.scale)) {
            return Ok(());
        }

        Err(crate::Error::Conversion(match self.increments {
            Self::LEAP_SECOND => "time: SQL Server has no leap seconds (second 60)".into(),
            n => format!(
                "time out of range: {n} increments of 10^-{} s is 24 hours or more",
                self.scale
            )
            .into(),
        }))
    }

    #[inline]
    /// Number of 10^-n second increments since midnight, where `n` is defined
    /// in [`scale`].
    ///
    /// [`scale`]: #method.scale
    pub fn increments(self) -> u64 {
        self.increments
    }

    #[inline]
    /// The accuracy of the increments.
    pub fn scale(self) -> u8 {
        self.scale
    }

    #[inline]
    /// Length of the field in number of bytes.
    pub(crate) fn len(self) -> crate::Result<u8> {
        Ok(match self.scale {
            0..=2 => 3,
            3..=4 => 4,
            5..=7 => 5,
            _ => {
                return Err(crate::Error::Protocol(
                    format!("timen: invalid scale {}", self.scale).into(),
                ))
            }
        })
    }

    pub(crate) async fn decode<R>(src: &mut R, n: usize, rlen: usize) -> crate::Result<Time>
    where
        R: SqlReadBytes + Unpin,
    {
        let val = match (n, rlen) {
            (0..=2, 3) => {
                let hi = src.read_u16_le().await? as u64;
                let lo = src.read_u8().await? as u64;

                hi | lo << 16
            }
            (3..=4, 4) => src.read_u32_le().await? as u64,
            (5..=7, 5) => {
                let hi = src.read_u32_le().await? as u64;
                let lo = src.read_u8().await? as u64;

                hi | lo << 32
            }
            _ => {
                return Err(crate::Error::Protocol(
                    format!("timen: invalid length {}", n).into(),
                ))
            }
        };

        Ok(Time {
            increments: val,
            scale: n as u8,
        })
    }
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl Encode<BytesMut> for Time {
    fn encode(self, dst: &mut BytesMut) -> crate::Result<()> {
        self.validate()?;

        // Under 24 hours, the increments fit the scale's 3, 4 or 5 bytes.
        let len = usize::from(self.len()?);
        dst.extend_from_slice(&self.increments.to_le_bytes()[..len]);

        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
/// A presentation of `datetime2` type in the server.
///
/// # Warning
///
/// It isn't recommended to use this type directly. For dealing with
/// `datetime2`, use the `time` feature of this crate and its `PrimitiveDateTime`
/// type.
pub struct DateTime2 {
    date: Date,
    time: Time,
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl DateTime2 {
    /// Construct a new `DateTime2` from the date and time components.
    pub fn new(date: Date, time: Time) -> Self {
        Self { date, time }
    }

    /// The date component.
    pub fn date(self) -> Date {
        self.date
    }

    /// The time component.
    pub fn time(self) -> Time {
        self.time
    }

    pub(crate) async fn decode<R>(src: &mut R, n: usize, rlen: usize) -> crate::Result<Self>
    where
        R: SqlReadBytes + Unpin,
    {
        let time = Time::decode(src, n, rlen).await?;

        let mut bytes = [0u8; 4];
        src.read_exact(&mut bytes[..3]).await?;
        let date = Date::new(LittleEndian::read_u32(&bytes));

        Ok(Self::new(date, time))
    }
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl Encode<BytesMut> for DateTime2 {
    fn encode(self, dst: &mut BytesMut) -> crate::Result<()> {
        self.time.encode(dst)?;
        self.date.encode(dst)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
/// A presentation of `datetimeoffset` type in the server.
///
/// # Warning
///
/// It isn't recommended to use this type directly. For dealing with
/// `datetimeoffset`, use the `time` feature of this crate and its `OffsetDateTime`
/// type with the correct timezone.
pub struct DateTimeOffset {
    datetime2: DateTime2,
    offset: i16,
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl DateTimeOffset {
    /// Construct a new `DateTimeOffset` from a `datetime2`, offset marking
    /// number of minutes from UTC.
    pub fn new(datetime2: DateTime2, offset: i16) -> Self {
        Self { datetime2, offset }
    }

    /// The date and time part.
    pub fn datetime2(self) -> DateTime2 {
        self.datetime2
    }

    /// Number of minutes from UTC.
    pub fn offset(self) -> i16 {
        self.offset
    }

    pub(crate) async fn decode<R>(src: &mut R, n: usize, rlen: u8) -> crate::Result<Self>
    where
        R: SqlReadBytes + Unpin,
    {
        let datetime2 = DateTime2::decode(src, n, rlen as usize).await?;
        let offset = src.read_i16_le().await?;

        Ok(Self { datetime2, offset })
    }
}

#[cfg(feature = "tds73")]
#[cfg_attr(docsrs, doc(cfg(feature = "tds73")))]
impl Encode<BytesMut> for DateTimeOffset {
    fn encode(self, dst: &mut BytesMut) -> crate::Result<()> {
        // MS-TDS 2.2.5.5.1.8: the offset MUST be within -840..=840 minutes.
        match self.offset {
            -840..=840 => {}
            o => {
                return Err(crate::Error::Conversion(
                    format!("datetimeoffset: offset of {o} minutes is outside -14:00 to +14:00")
                        .into(),
                ))
            }
        }

        self.datetime2.encode(dst)?;
        dst.put_i16_le(self.offset);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

    #[test]
    fn datetime_accessors() {
        let dt = DateTime::new(-100, 12345);
        assert_eq!(dt.days(), -100);
        assert_eq!(dt.seconds_fragments(), 12345);
    }

    #[tokio::test]
    async fn datetime_round_trip_including_pre_1900() {
        for dt in [
            DateTime::new(0, 0),
            DateTime::new(200, 3000),
            // 1753-01-01 and 9999-12-31 23:59:59.997, the type's bounds.
            DateTime::new(-53690, 0),
            DateTime::new(2958463, 25919999),
        ] {
            let mut buf = BytesMut::new();
            dt.encode(&mut buf).unwrap();
            let decoded = DateTime::decode(&mut buf.into_sql_read_bytes())
                .await
                .unwrap();
            assert_eq!(decoded, dt);
        }
    }

    #[tokio::test]
    async fn decode_rejects_a_time_past_midnight() {
        let mut buf = BytesMut::new();
        DateTime::new(0, 25920000).encode(&mut buf).unwrap();
        let err = DateTime::decode(&mut buf.into_sql_read_bytes()).await;
        assert!(matches!(err, Err(crate::Error::Protocol(_))), "{err:?}");

        let mut buf = BytesMut::new();
        SmallDateTime::new(0, 1440).encode(&mut buf).unwrap();
        let err = SmallDateTime::decode(&mut buf.into_sql_read_bytes()).await;
        assert!(matches!(err, Err(crate::Error::Protocol(_))), "{err:?}");
    }

    #[test]
    fn datetime_millis_round_half_up() {
        let ms = |ticks| DateTime::new(0, ticks).millis_of_day().unwrap();
        // .003, .007, .010 and .997 are stored as 1, 2, 3 and 299 ticks.
        assert_eq!(ms(1), 3);
        assert_eq!(ms(2), 7);
        assert_eq!(ms(3), 10);
        assert_eq!(ms(299), 997);
        assert_eq!(ms(25919999), 86_399_997);
        assert!(DateTime::new(0, 25920000).millis_of_day().is_err());
        assert!(DateTime::new(0, u32::MAX).millis_of_day().is_err());
    }

    #[test]
    fn smalldatetime_accessors() {
        let dt = SmallDateTime::new(100, 200);
        assert_eq!(dt.days(), 100);
        assert_eq!(dt.seconds_fragments(), 200);
    }

    #[tokio::test]
    async fn smalldatetime_round_trip() {
        let dt = SmallDateTime::new(65535, 1439);
        let mut buf = BytesMut::new();
        dt.encode(&mut buf).unwrap();
        let decoded = SmallDateTime::decode(&mut buf.into_sql_read_bytes())
            .await
            .unwrap();
        assert_eq!(decoded, dt);
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn date_accessor_and_new() {
        let date = Date::new(730119);
        assert_eq!(date.days(), 730119);
    }

    #[cfg(feature = "tds73")]
    #[test]
    #[should_panic]
    fn date_new_panics_on_overflow() {
        // Anything not representable in three bytes must panic.
        Date::new(0x0100_0000);
    }

    #[cfg(feature = "tds73")]
    #[tokio::test]
    async fn date_round_trip() {
        // 3_652_058 is 9999-12-31.
        for days in [0u32, 1, 730119, 3_652_058] {
            let date = Date::new(days);
            let mut buf = BytesMut::new();
            date.encode(&mut buf).unwrap();
            assert_eq!(buf.len(), 3);
            let decoded = Date::decode(&mut buf.into_sql_read_bytes()).await.unwrap();
            assert_eq!(decoded, date);
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn dates_outside_year_1_to_9999_are_encode_errors() {
        // Past 9999-12-31, the top of three bytes, and a day before year 1.
        for date in [
            Date::new(3_652_059),
            Date::new(0x00ff_ffff),
            Date::from_signed_days(-1),
        ] {
            let err = date.encode(&mut BytesMut::new()).unwrap_err();
            assert!(matches!(err, crate::Error::Conversion(_)), "{date:?}");

            let dt2 = DateTime2::new(date, Time::new(0, 7));
            let err = dt2.encode(&mut BytesMut::new()).unwrap_err();
            assert!(matches!(err, crate::Error::Conversion(_)), "{date:?}");
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn time_accessors_and_len() {
        let time = Time::new(1234, 5);
        assert_eq!(time.increments(), 1234);
        assert_eq!(time.scale(), 5);
        assert_eq!(time.len().unwrap(), 5);

        assert_eq!(Time::new(0, 0).len().unwrap(), 3);
        assert_eq!(Time::new(0, 3).len().unwrap(), 4);
        assert!(Time::new(0, 8).len().is_err());
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn time_partial_eq_across_scales() {
        // 1 second expressed at two different scales must compare equal.
        assert_eq!(Time::new(100, 2), Time::new(10_000_000, 7));
        assert_ne!(Time::new(100, 2), Time::new(200, 2));
    }

    #[cfg(feature = "tds73")]
    #[tokio::test]
    async fn time_round_trip_all_len_buckets() {
        for (increments, scale) in [(255u64, 2u8), (65535, 4), (16_777_215, 7)] {
            let time = Time::new(increments, scale);
            let rlen = time.len().unwrap();
            let mut buf = BytesMut::new();
            time.encode(&mut buf).unwrap();
            let decoded = Time::decode(
                &mut buf.into_sql_read_bytes(),
                scale as usize,
                rlen as usize,
            )
            .await
            .unwrap();
            assert_eq!(decoded, time);
        }
    }

    #[cfg(feature = "tds73")]
    #[tokio::test]
    async fn time_round_trip_high_bytes_set() {
        // Values whose most-significant byte (the byte handled by the
        // `lo << 16` / `lo << 32` shift in `decode` and the `>> 16` / `>> 32`
        // shift in `encode`) is non-zero. This distinguishes:
        //   * decode `<< N` from `>> N` (the latter zeroes an `u8`), and
        //   * encode `>> N` from `<< N` (the latter zeroes the byte written).
        // The 16-bit / 32-bit low halves and the shifted high byte occupy
        // disjoint bit ranges, so `|` vs `^` cannot be distinguished here.
        // Both are under 24 hours.
        for (increments, scale) in [(0x0083_1234u64, 2u8), (0x00AB_1234_5678u64, 7)] {
            let time = Time::new(increments, scale);
            let rlen = time.len().unwrap();

            let mut buf = BytesMut::new();
            time.encode(&mut buf).unwrap();

            let decoded = Time::decode(
                &mut buf.into_sql_read_bytes(),
                scale as usize,
                rlen as usize,
            )
            .await
            .unwrap();

            assert_eq!(decoded, time);
            assert_eq!(decoded.increments(), increments);
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn times_from_24_hours_up_are_encode_errors() {
        // Over the field's 3/4/5 bytes (these asserted), within them but at or
        // past 24 h, and the chrono leap second.
        for time in [
            Time::new(1 << 24, 0),
            Time::new(1 << 32, 4),
            Time::new(u64::MAX, 7),
            Time::new(86_400, 0),
            Time::new(8_640_000, 2),
            Time::new(864_000_000_000, 7),
            Time::new(Time::LEAP_SECOND, 7),
        ] {
            let err = time.encode(&mut BytesMut::new()).unwrap_err();
            assert!(matches!(err, crate::Error::Conversion(_)), "{time:?}");

            let dt2 = DateTime2::new(Date::new(1), time);
            let err = dt2.encode(&mut BytesMut::new()).unwrap_err();
            assert!(matches!(err, crate::Error::Conversion(_)), "{time:?}");
        }

        for scale in 0..=7 {
            let last = 86_400 * 10u64.pow(scale) - 1;
            Time::new(last, scale as u8)
                .encode(&mut BytesMut::new())
                .unwrap();
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn datetimeoffset_offsets_beyond_14_hours_are_encode_errors() {
        let dt2 = DateTime2::new(Date::new(1), Time::new(0, 7));
        for offset in [-841, 841, i16::MAX] {
            let err = DateTimeOffset::new(dt2, offset)
                .encode(&mut BytesMut::new())
                .unwrap_err();
            assert!(matches!(err, crate::Error::Conversion(_)), "{offset}");
        }
        for offset in [-840, 0, 840] {
            DateTimeOffset::new(dt2, offset)
                .encode(&mut BytesMut::new())
                .unwrap();
        }
    }

    #[cfg(feature = "tds73")]
    #[tokio::test]
    async fn time_decode_invalid_length_errors() {
        let mut buf = BytesMut::new();
        buf.put_u8(0);
        // scale/length combination not one of the accepted pairs.
        let err = Time::decode(&mut buf.into_sql_read_bytes(), 0, 4).await;
        assert!(err.is_err());
    }

    #[cfg(feature = "tds73")]
    #[tokio::test]
    async fn datetime2_round_trip_and_accessors() {
        let dt2 = DateTime2::new(Date::new(730119), Time::new(222, 7));
        assert_eq!(dt2.date(), Date::new(730119));
        assert_eq!(dt2.time(), Time::new(222, 7));

        let rlen = dt2.time().len().unwrap();
        let mut buf = BytesMut::new();
        dt2.encode(&mut buf).unwrap();
        let decoded = DateTime2::decode(&mut buf.into_sql_read_bytes(), 7, rlen as usize)
            .await
            .unwrap();
        assert_eq!(decoded, dt2);
    }

    #[cfg(feature = "tds73")]
    #[tokio::test]
    async fn datetimeoffset_round_trip_and_accessors() {
        let dt2 = DateTime2::new(Date::new(730119), Time::new(222, 7));
        let dto = DateTimeOffset::new(dt2, -120);
        assert_eq!(dto.datetime2(), dt2);
        assert_eq!(dto.offset(), -120);

        let rlen = dto.datetime2().time().len().unwrap();
        let mut buf = BytesMut::new();
        dto.encode(&mut buf).unwrap();
        let decoded = DateTimeOffset::decode(&mut buf.into_sql_read_bytes(), 7, rlen)
            .await
            .unwrap();
        assert_eq!(decoded, dto);
    }
}
