//! Mappings between TDS and and time crate types (with `time` feature flag
//! enabled).
//!
//! The time library offers better ergonomy and are highly recommended if
//! needing to modify and deal with date and time in SQL Server.

#[cfg(feature = "tds73")]
use std::time::Duration;
pub use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::tds::codec::ColumnData;

#[inline]
fn from_days(days: i64, start_year: i32) -> Date {
    // Use the signed `time::Duration` so that negative day offsets (dates
    // before `start_year`, e.g. `datetime` values prior to 1900) do not
    // overflow. Casting a negative day count into an unsigned type and
    // multiplying it out panics with "multiply with overflow".
    //
    // `days` ultimately comes from untrusted server bytes, so a malformed value
    // can land outside the range `time::Date` can represent. Use `checked_add`
    // and clamp to the type's bounds instead of panicking with "resulting value
    // is out of range".
    let base = Date::from_calendar_date(start_year, Month::January, 1).unwrap();
    base.checked_add(time::Duration::days(days))
        .unwrap_or(if days < 0 { Date::MIN } else { Date::MAX })
}

/// Convert a server-supplied fractional-seconds `increments` at the given
/// `scale` into nanoseconds without panicking. `scale` and `increments` are
/// untrusted; a `scale > 9` would otherwise underflow `9 - scale`, and a large
/// `increments` would overflow the multiply.
#[inline]
#[cfg(feature = "tds73")]
fn nanos_from_increments(increments: u64, scale: u8) -> u64 {
    let pow = 9u32.saturating_sub(scale as u32);
    increments.saturating_mul(10u64.saturating_pow(pow))
}

fn from_millis(ms: u32) -> crate::Result<Time> {
    let err = || crate::Error::Conversion(format!("{ms} ms is past midnight").into());
    let hour = u8::try_from(ms / 3_600_000).map_err(|_| err())?;
    let (min, sec, milli) = (ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);

    Time::from_hms_milli(hour, min as u8, sec as u8, milli as u16).map_err(|_| err())
}

fn from_datetime(dt: super::DateTime) -> crate::Result<PrimitiveDateTime> {
    let time = from_millis(dt.millis_of_day()?)?;
    Ok(from_days(dt.days() as i64, 1900).with_time(time))
}

#[cfg(feature = "tds73")]
fn from_smalldatetime(dt: super::SmallDateTime) -> crate::Result<PrimitiveDateTime> {
    let time = from_millis(dt.minutes_of_day()? * 60_000)?;
    Ok(from_days(dt.days() as i64, 1900).with_time(time))
}

impl<'a> crate::FromSql<'a> for PrimitiveDateTime {
    fn from_sql(data: &'a ColumnData<'static>) -> crate::Result<Option<Self>> {
        match data {
            ColumnData::DateTime(dt) => dt.map(from_datetime).transpose(),
            #[cfg(feature = "tds73")]
            ColumnData::SmallDateTime(dt) => dt.map(from_smalldatetime).transpose(),
            #[cfg(feature = "tds73")]
            ColumnData::DateTime2(dt) => Ok(dt.map(|dt| {
                let ns = nanos_from_increments(dt.time.increments, dt.time.scale);
                from_days(dt.date.days() as i64, 1)
                    .with_time(Time::MIDNIGHT + Duration::from_nanos(ns))
            })),
            _ => Err(crate::Error::Conversion(
                format!("cannot interpret {:?} as an PrimitiveDateTime value", data).into(),
            )),
        }
    }
}

impl crate::FromSqlOwned for PrimitiveDateTime {
    fn from_sql_owned(data: ColumnData<'static>) -> crate::Result<Option<Self>> {
        <Self as crate::FromSql>::from_sql(&data)
    }
}

#[inline]
fn to_days(date: Date, start_year: i32) -> i64 {
    (date - Date::from_calendar_date(start_year, Month::January, 1).unwrap()).whole_days()
}

#[inline]
#[cfg(not(feature = "tds73"))]
fn to_sec_fragments(from: Time) -> i64 {
    let nanos: i64 = (from - Time::from_hms(0, 0, 0).unwrap())
        .whole_nanoseconds()
        .try_into()
        .unwrap();

    nanos * 300 / (1e9 as i64)
}

#[cfg(feature = "tds73")]
from_sql!(
    Time:
        ColumnData::Time(ref time) => time.map(|time| {
            let ns = nanos_from_increments(time.increments, time.scale);
            Time::from_hms(0,0,0).unwrap() + Duration::from_nanos(ns)
        });
    Date:
        ColumnData::Date(ref date) => date.map(|date| from_days(date.days() as i64, 1));
    OffsetDateTime:
        ColumnData::DateTimeOffset(ref dto) => dto.map(|dto| {
            let date = from_days(dto.datetime2.date.days() as i64, 1);
            let dt = dto.datetime2;

            let time = Time::from_hms(0,0,0).unwrap()
                + Duration::from_nanos(nanos_from_increments(dt.time.increments, dt.time.scale));

            // A malformed server offset outside ±14h is not representable by
            // `UtcOffset`; fall back to UTC rather than panicking.
            let offset = UtcOffset::from_whole_seconds(dto.offset as i32 * 60)
                .unwrap_or(UtcOffset::UTC);

            // Nor past `Date::MAX` locally, where a malformed date is clamped
            // (`to_offset` panics there).
            let utc = date.with_time(time);
            match utc.checked_add(time::Duration::seconds(offset.whole_seconds().into())) {
                Some(local) => local.assume_offset(offset),
                None => utc.assume_utc(),
            }
        })
);

#[cfg(feature = "tds73")]
to_sql!(self_,
        Date: (ColumnData::Date, super::Date::from_signed_days(to_days(*self_, 1)));
        Time: (ColumnData::Time, {
            let nanos: u64 = (*self_ - Time::from_hms(0, 0, 0).unwrap()).whole_nanoseconds().try_into().unwrap();
            let increments = nanos / 100;

            super::Time {increments, scale: 7}
        });
        PrimitiveDateTime: (ColumnData::DateTime2, {
            let time = self_.time();
            let nanos: u64 = (time - Time::from_hms(0, 0, 0).unwrap()).whole_nanoseconds().try_into().unwrap();
            let increments = nanos / 100;

            let date = super::Date::from_signed_days(to_days(self_.date(), 1));
            let time = super::Time {increments, scale: 7};

            super::DateTime2::new(date, time)
        });
        OffsetDateTime: (ColumnData::DateTimeOffset, {
            // datetimeoffset stores whole minutes: one with seconds is
            // rejected instead of cut. Within ±25:59:59, so i16 holds it.
            let tz = self_.offset();
            let offset = match tz.seconds_past_minute() {
                0 => (tz.whole_seconds() / 60) as i16,
                _ => super::DateTimeOffset::OFFSET_WITH_SECONDS,
            };

            // From the timestamp: `to_offset(UTC)` panics when the UTC date
            // passes year 9999.
            const DAY_NANOS: i128 = 86_400_000_000_000;
            const DAYS_TO_UNIX_EPOCH: i64 = 719_162;
            let utc = self_.unix_timestamp_nanos();

            let date = super::Date::from_signed_days(DAYS_TO_UNIX_EPOCH + utc.div_euclid(DAY_NANOS) as i64);
            let time = super::Time { increments: (utc.rem_euclid(DAY_NANOS) / 100) as u64, scale: 7 };

            super::DateTimeOffset::new(super::DateTime2::new(date, time), offset)
        });
);

#[cfg(not(feature = "tds73"))]
to_sql!(self_,
        PrimitiveDateTime: (ColumnData::DateTime, {
            let date = self_.date();
            let time = self_.time();

            let days = to_days(date, 1900) as i32;
            let seconds_fragments = to_sec_fragments(time);

            super::DateTime::new(days, seconds_fragments as u32)
        });
);

#[cfg(test)]
mod tests {
    use super::*;

    // Regression test for #316: a `datetime` value with a date before 1900 has
    // a negative day offset from the 1900 base date. This must round-trip
    // without a "multiply with overflow" panic.
    #[test]
    fn from_days_handles_negative_offsets() {
        // 1899-12-31 is one day before the 1900 base date.
        assert_eq!(
            from_days(-1, 1900),
            Date::from_calendar_date(1899, Month::December, 31).unwrap()
        );

        // A date well before 1900, at the lower edge of the `datetime` range.
        let expected = Date::from_calendar_date(1850, Month::January, 1).unwrap();
        let days = to_days(expected, 1900);
        assert!(
            days < 0,
            "expected a negative day offset for pre-1900 dates"
        );

        // Rebuilding from the (negative) day offset must not overflow.
        assert_eq!(from_days(days, 1900), expected);
    }

    // Exercise the full decode path (`DateTime` -> `PrimitiveDateTime`) for a
    // pre-1900 value, matching what happens when reading a `datetime` column.
    #[test]
    fn datetime_before_1900_decodes() {
        let expected_date = Date::from_calendar_date(1850, Month::January, 1).unwrap();
        let days = to_days(expected_date, 1900) as i32;

        let decoded = from_datetime(crate::tds::time::DateTime::new(days, 0)).unwrap();

        assert_eq!(decoded.date(), expected_date);
        assert_eq!(decoded.time(), Time::from_hms(0, 0, 0).unwrap());
    }

    #[test]
    fn from_days_clamps_on_overflow() {
        // A day offset far outside the representable `time::Date` range forces
        // the `checked_add` fallback. Positive overflow clamps to MAX, negative
        // to MIN. Pins the `days < 0` sign test (vs `==` / `>`).
        assert_eq!(from_days(10_000_000, 1), Date::MAX);
        assert_eq!(from_days(-10_000_000, 1), Date::MIN);
    }

    #[test]
    fn datetime_rounds_to_the_millisecond() {
        let get = |ticks| {
            from_datetime(crate::tds::time::DateTime::new(0, ticks))
                .unwrap()
                .time()
        };
        assert_eq!(get(300), Time::from_hms(0, 0, 1).unwrap());
        assert_eq!(get(299), Time::from_hms_milli(0, 0, 0, 997).unwrap());
        assert_eq!(get(2), Time::from_hms_milli(0, 0, 0, 7).unwrap());
        assert_eq!(
            get(25919999),
            Time::from_hms_milli(23, 59, 59, 997).unwrap()
        );
        assert!(from_millis(86_400_000).is_err());
        assert!(from_millis(u32::MAX).is_err());
    }

    #[test]
    fn out_of_range_times_are_conversion_errors() {
        use crate::tds::time::{DateTime, SmallDateTime};
        use crate::{FromSql, FromSqlOwned};

        let dt = ColumnData::DateTime(Some(DateTime::new(0, 300 * 86_400)));
        assert!(matches!(
            PrimitiveDateTime::from_sql(&dt),
            Err(crate::Error::Conversion(_))
        ));
        assert!(PrimitiveDateTime::from_sql_owned(dt).is_err());

        // Used to wrap silently to 00:00.
        let sdt = ColumnData::SmallDateTime(Some(SmallDateTime::new(0, 1440)));
        assert!(PrimitiveDateTime::from_sql(&sdt).is_err());
    }

    #[cfg(not(feature = "tds73"))]
    #[test]
    fn to_sec_fragments_converts() {
        // One second == 300 sec-fragments (1/300 s units).
        assert_eq!(to_sec_fragments(Time::from_hms(0, 0, 1).unwrap()), 300);
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn time_from_sql_and_back() {
        use crate::FromSql;

        // 12:34:56 as 100ns increments since midnight, scale 7.
        let expected = Time::from_hms(12, 34, 56).unwrap();
        let nanos: u64 = (expected - Time::from_hms(0, 0, 0).unwrap())
            .whole_nanoseconds()
            .try_into()
            .unwrap();
        let increments: u64 = nanos / 100;

        let tds_time = super::super::Time::new(increments, 7);
        let data = ColumnData::Time(Some(tds_time));

        let decoded = Time::from_sql(&data).unwrap().unwrap();
        assert_eq!(decoded, expected);

        // Round trip back through ToSql.
        use crate::ToSql;
        let round_tripped = decoded.to_sql();
        match round_tripped {
            ColumnData::Time(Some(t)) => assert_eq!(t.increments, increments),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn date_from_sql_and_back() {
        use crate::{FromSql, ToSql};

        let expected = Date::from_calendar_date(2020, Month::June, 15).unwrap();
        let days = to_days(expected, 1) as u32;

        let data = ColumnData::Date(Some(super::super::Date::new(days)));
        let decoded = Date::from_sql(&data).unwrap().unwrap();
        assert_eq!(decoded, expected);

        match decoded.to_sql() {
            ColumnData::Date(Some(d)) => assert_eq!(d.days(), days),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn primitive_datetime_from_datetime2_and_back() {
        use crate::{FromSql, ToSql};

        let date = Date::from_calendar_date(2020, Month::June, 15).unwrap();
        let time = Time::from_hms(1, 2, 3).unwrap();
        let expected = PrimitiveDateTime::new(date, time);

        let days = to_days(date, 1) as u32;
        let nanos: u64 = (time - Time::from_hms(0, 0, 0).unwrap())
            .whole_nanoseconds()
            .try_into()
            .unwrap();
        let increments = nanos / 100;

        let dt2 = super::super::DateTime2::new(
            super::super::Date::new(days),
            super::super::Time::new(increments, 7),
        );
        let data = ColumnData::DateTime2(Some(dt2));

        let decoded = PrimitiveDateTime::from_sql(&data).unwrap().unwrap();
        assert_eq!(decoded, expected);

        match decoded.to_sql() {
            ColumnData::DateTime2(Some(dt)) => {
                assert_eq!(dt.date.days(), days);
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn primitive_datetime_from_smalldatetime_and_datetime() {
        use crate::FromSql;

        // SmallDateTime path.
        let sdt = crate::tds::time::SmallDateTime::new(1, 30); // 30 minutes past midnight on day 1 (1900-01-02)
        let data = ColumnData::SmallDateTime(Some(sdt));
        let decoded = PrimitiveDateTime::from_sql(&data).unwrap().unwrap();
        assert_eq!(decoded.date(), from_days(1, 1900));
        assert_eq!(decoded.time(), Time::from_hms(0, 30, 0).unwrap());

        // DateTime path.
        let dt = crate::tds::time::DateTime::new(1, 0);
        let data = ColumnData::DateTime(Some(dt));
        let decoded = PrimitiveDateTime::from_sql(&data).unwrap().unwrap();
        assert_eq!(decoded.date(), from_days(1, 1900));
    }

    /// A malformed date is clamped to `Date::MAX`, past which a positive
    /// offset made `to_offset` panic.
    #[cfg(feature = "tds73")]
    #[test]
    fn offset_date_time_past_the_last_local_date_keeps_utc() {
        use crate::FromSql;

        let dt2 = super::super::DateTime2::new(
            super::super::Date::new(0xff_ffff),
            super::super::Time::new(23 * 3600 * 10_000_000, 7),
        );
        let dto = super::super::DateTimeOffset::new(dt2, 14 * 60);
        let data = ColumnData::DateTimeOffset(Some(dto));
        let decoded = OffsetDateTime::from_sql(&data).unwrap().unwrap();

        let utc = from_days(0xff_ffff, 1).with_time(Time::from_hms(23, 0, 0).unwrap());
        assert_eq!(decoded, utc.assume_utc());
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn offset_date_time_from_sql_and_back() {
        use crate::{FromSql, ToSql};

        let date = Date::from_calendar_date(2020, Month::June, 15).unwrap();
        let time = Time::from_hms(1, 2, 3).unwrap();
        let days = to_days(date, 1) as u32;
        let nanos: u64 = (time - Time::from_hms(0, 0, 0).unwrap())
            .whole_nanoseconds()
            .try_into()
            .unwrap();
        let increments = nanos / 100;

        let dt2 = super::super::DateTime2::new(
            super::super::Date::new(days),
            super::super::Time::new(increments, 7),
        );
        let dto = super::super::DateTimeOffset::new(dt2, 60); // +1h offset

        let data = ColumnData::DateTimeOffset(Some(dto));
        let decoded = OffsetDateTime::from_sql(&data).unwrap().unwrap();

        assert_eq!(
            decoded.offset(),
            UtcOffset::from_whole_seconds(3600).unwrap()
        );

        match decoded.to_sql() {
            ColumnData::DateTimeOffset(Some(round_tripped)) => {
                assert_eq!(round_tripped.offset, 60);
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[cfg(feature = "tds73")]
    fn encode(data: ColumnData<'_>) -> crate::Result<()> {
        use crate::tds::codec::Encode;

        let mut buf = bytes::BytesMut::new();
        data.encode(&mut crate::BytesMutWithTypeInfo::new(&mut buf))
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn dates_outside_year_1_to_9999_are_encode_errors() {
        use crate::ToSql;

        let noon = Time::from_hms(12, 0, 0).unwrap();
        for year in [0, -5, -9999] {
            let date = Date::from_calendar_date(year, Month::January, 1).unwrap();
            let dt = PrimitiveDateTime::new(date, noon);
            for data in [date.to_sql(), dt.to_sql(), dt.assume_utc().to_sql()] {
                let err = encode(data).unwrap_err();
                assert!(
                    matches!(err, crate::Error::Conversion(_)),
                    "{year}: {err:?}"
                );
            }
        }

        // In range locally, out of range in UTC, which is what datetimeoffset
        // stores. The upper one used to panic in `to_offset(UTC)`.
        let edges = [
            (1, Month::January, 1, 0, 30, 1),
            (9999, Month::December, 31, 23, 30, -1),
        ];
        for (y, m, d, h, min, offset_h) in edges {
            let local = PrimitiveDateTime::new(
                Date::from_calendar_date(y, m, d).unwrap(),
                Time::from_hms(h, min, 0).unwrap(),
            );
            let dto = local.assume_offset(UtcOffset::from_hms(offset_h, 0, 0).unwrap());
            let err = encode(dto.to_sql()).unwrap_err();
            assert!(matches!(err, crate::Error::Conversion(_)), "{dto}: {err:?}");
        }
    }

    #[cfg(feature = "tds73")]
    #[test]
    fn offset_date_time_to_sql_stores_the_utc_date_and_time() {
        use crate::ToSql;

        // Either side of the Unix epoch, with a negative and a positive offset.
        let cases = [
            ((2024, Month::March, 4), (5, 6, 7, 123_456_700), (-5, -45)),
            ((1900, Month::January, 1), (0, 0, 0, 500_000_000), (14, 0)),
        ];
        for ((y, mo, d), (h, mi, s, ns), (oh, om)) in cases {
            let local = PrimitiveDateTime::new(
                Date::from_calendar_date(y, mo, d).unwrap(),
                Time::from_hms_nano(h, mi, s, ns).unwrap(),
            );
            let offset = UtcOffset::from_hms(oh, om, 0).unwrap();
            let dto = local.assume_offset(offset);
            let utc = dto.to_offset(UtcOffset::UTC);
            let utc_nanos: u64 = (utc.time() - Time::MIDNIGHT)
                .whole_nanoseconds()
                .try_into()
                .unwrap();

            match dto.to_sql() {
                ColumnData::DateTimeOffset(Some(v)) => {
                    assert_eq!(v.datetime2.date.days() as i64, to_days(utc.date(), 1));
                    assert_eq!(v.datetime2.time.increments, utc_nanos / 100);
                    assert_eq!(v.offset as i32, offset.whole_seconds() / 60);
                }
                other => panic!("unexpected: {:?}", other),
            }
        }
    }

    /// Cut to whole minutes, +01:00:30 was stored as +01:00, 30 s off.
    #[cfg(feature = "tds73")]
    #[test]
    fn offsets_with_seconds_are_encode_errors() {
        use crate::ToSql;

        let local = PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::May, 6).unwrap(),
            Time::from_hms(1, 2, 3).unwrap(),
        );

        for (h, m, s) in [(0, 0, 30), (0, 0, -30), (1, 0, 30), (-1, -30, -1)] {
            let dto = local.assume_offset(UtcOffset::from_hms(h, m, s).unwrap());
            let err = encode(dto.to_sql()).unwrap_err();
            assert!(matches!(err, crate::Error::Conversion(_)), "{dto}: {err:?}");
        }

        for (h, m) in [(-14, 0), (-1, -30), (14, 0)] {
            let dto = local.assume_offset(UtcOffset::from_hms(h, m, 0).unwrap());
            encode(dto.to_sql()).unwrap();
        }
    }

    // `ColumnData::Time`/`Date`/`DateTimeOffset` and their `time`-crate
    // `FromSql` impls only exist with the `tds73` feature.
    #[cfg(feature = "tds73")]
    #[test]
    fn from_sql_null_variants_return_none_tds73() {
        use crate::FromSql;

        assert_eq!(Time::from_sql(&ColumnData::Time(None)).unwrap(), None);
        assert_eq!(Date::from_sql(&ColumnData::Date(None)).unwrap(), None);
        assert_eq!(
            OffsetDateTime::from_sql(&ColumnData::DateTimeOffset(None)).unwrap(),
            None
        );
    }

    #[test]
    fn primitive_datetime_from_sql_null_returns_none() {
        use crate::FromSql;

        assert_eq!(
            PrimitiveDateTime::from_sql(&ColumnData::DateTime(None)).unwrap(),
            None
        );
    }
}
