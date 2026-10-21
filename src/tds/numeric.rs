//! Representations of numeric types.

use super::codec::Encode;
use crate::{sql_read_bytes::SqlReadBytes, Error};
#[cfg(feature = "bigdecimal")]
#[cfg_attr(docsrs, doc(cfg(feature = "bigdecimal")))]
pub use bigdecimal::{num_bigint::BigInt, BigDecimal};
use bytes::{BufMut, BytesMut};
#[cfg(feature = "rust_decimal")]
#[cfg_attr(docsrs, doc(cfg(feature = "rust_decimal")))]
pub use rust_decimal::Decimal;
use std::cmp::{Ordering, PartialEq};
use std::fmt::{self, Debug, Display, Formatter};

/// Represent a sql Decimal / Numeric type. It is stored in a i128 and has a
/// maximum precision of 38 decimals.
///
/// A recommended way of dealing with numeric values is by enabling the
/// `rust_decimal` feature and using its `Decimal` type instead.
#[derive(Copy, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "NumericParts"))]
pub struct Numeric {
    value: i128,
    scale: u8,
}

/// `Numeric`'s serde form, checked on the way in: the arithmetic in
/// `int_part`, `dec_part` and `Display` assumes a scale of at most 38.
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
#[serde(rename = "Numeric")]
struct NumericParts {
    value: i128,
    scale: u8,
}

#[cfg(feature = "serde")]
impl TryFrom<NumericParts> for Numeric {
    type Error = String;

    fn try_from(n: NumericParts) -> Result<Self, String> {
        match n.scale {
            0..=38 => Ok(Numeric {
                value: n.value,
                scale: n.scale,
            }),
            s => Err(format!("Numeric scale {s} is over 38")),
        }
    }
}

impl Numeric {
    /// Creates a new Numeric value.
    ///
    /// # Panic
    /// It will panic if the scale exceeds 38.
    pub fn new_with_scale(value: i128, scale: u8) -> Self {
        // SQL Server allows a maximum precision of 38, and scale may equal
        // precision (e.g. `decimal(38, 38)`), so scale 38 is valid; 10^38 still
        // fits in i128.
        assert!(scale <= 38);

        Numeric { value, scale }
    }

    /// Extract the decimal part.
    pub fn dec_part(self) -> i128 {
        let scale = self.pow_scale();
        self.value - (self.value / scale) * scale
    }

    /// Extract the integer part.
    pub fn int_part(self) -> i128 {
        self.value / self.pow_scale()
    }

    #[inline]
    fn pow_scale(self) -> i128 {
        10i128.pow(self.scale as u32)
    }

    /// The scale (where is the decimal point) of the value.
    #[inline]
    pub fn scale(self) -> u8 {
        self.scale
    }

    /// The internal integer value
    #[inline]
    pub fn value(self) -> i128 {
        self.value
    }

    /// The precision of the `Number` as a number of digits: the smallest `p`
    /// for which the value fits `decimal(p, scale)`.
    pub fn precision(self) -> u8 {
        let digits = self
            .value
            .unsigned_abs()
            .checked_ilog10()
            .map_or(0, |l| l + 1);

        // No leading zero digit: 0.5 is decimal(1, 1), as SQL Server types it.
        (digits as u8).max(self.scale).max(1)
    }

    pub(crate) fn len(self) -> u8 {
        match self.precision() {
            1..=9 => 5,
            10..=19 => 9,
            20..=28 => 13,
            _ => 17,
        }
    }

    pub(crate) async fn decode<R>(src: &mut R, scale: u8) -> crate::Result<Option<Self>>
    where
        R: SqlReadBytes + Unpin,
    {
        let len = src.read_u8().await? as usize;
        if len == 0 {
            return Ok(None);
        }

        if !matches!(len, 5 | 9 | 13 | 17) {
            return Err(invalid_length(len));
        }

        let mut buf = [0u8; 17];
        futures_util::io::AsyncReadExt::read_exact(src, &mut buf[..len]).await?;

        Self::from_wire(&buf[..len], scale).map(Some)
    }

    /// Builds a value from its wire form: a sign byte (0 negative, 1 positive)
    /// then a 4, 8, 12 or 16 byte little-endian magnitude.
    pub(crate) fn from_wire(bytes: &[u8], scale: u8) -> crate::Result<Self> {
        if scale > 38 {
            return Err(Error::Protocol(
                format!("decimal/numeric: invalid scale {scale}").into(),
            ));
        }

        let (&sign, magnitude) = bytes.split_first().ok_or_else(|| invalid_length(0))?;
        if !matches!(magnitude.len(), 4 | 8 | 12 | 16) {
            return Err(invalid_length(bytes.len()));
        }

        let mut le = [0u8; 16];
        le[..magnitude.len()].copy_from_slice(magnitude);

        // A legal decimal(38, s) magnitude is below 10^38 < i128::MAX.
        let value = i128::try_from(u128::from_le_bytes(le)).map_err(|_| {
            Error::Protocol("decimal/numeric: magnitude exceeds the representable range".into())
        })?;

        let value = match sign {
            0 => -value,
            1 => value,
            _ => return Err(Error::Protocol("decimal/numeric: invalid sign".into())),
        };

        Ok(Numeric { value, scale })
    }

    /// The nearest `f64`. Exact operands make one division correctly rounded;
    /// otherwise the decimal string is, by `str::parse`.
    pub(crate) fn to_f64(self) -> f64 {
        const POW10: [f64; 23] = [
            1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
            1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
        ];

        match POW10.get(self.scale as usize) {
            Some(pow) if self.value.unsigned_abs() < 1 << f64::MANTISSA_DIGITS => {
                self.value as f64 / pow
            }
            _ => self
                .to_string()
                .parse()
                .expect("Display is a float literal"),
        }
    }

    /// The nearest `f32`, as [`Numeric::to_f64`].
    pub(crate) fn to_f32(self) -> f32 {
        const POW10: [f32; 11] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10];

        match POW10.get(self.scale as usize) {
            Some(pow) if self.value.unsigned_abs() < 1 << f32::MANTISSA_DIGITS => {
                self.value as f32 / pow
            }
            _ => self
                .to_string()
                .parse()
                .expect("Display is a float literal"),
        }
    }
}

fn invalid_length(len: usize) -> Error {
    Error::Protocol(format!("decimal/numeric: invalid length of {len} received").into())
}

impl Encode<BytesMut> for Numeric {
    fn encode(self, dst: &mut BytesMut) -> crate::Result<()> {
        // decimal(38, s) holds at most 38 digits whatever the scale.
        if self.value.unsigned_abs() >= 10u128.pow(38) {
            return Err(Error::Conversion(
                "numeric value out of range: SQL Server's decimal holds at most 38 digits".into(),
            ));
        }

        // `len()` recomputes `precision()` via a division loop; compute it once.
        let len = self.len();
        dst.put_u8(len);

        if self.value < 0 {
            dst.put_u8(0);
        } else {
            dst.put_u8(1);
        }

        let value = self.value().abs();

        match len {
            5 => dst.put_u32_le(value as u32),
            9 => dst.put_u64_le(value as u64),
            13 => {
                dst.put_u64_le(value as u64);
                dst.put_u32_le((value >> 64) as u32)
            }
            _ => dst.put_u128_le(value as u128),
        }

        Ok(())
    }
}

impl Debug for Numeric {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "{}", self)
    }
}

impl Display for Numeric {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        let sign = if self.value < 0 { "-" } else { "" };
        let abs = self.value.unsigned_abs();

        if self.scale == 0 {
            return write!(f, "{sign}{abs}");
        }

        let pow = 10u128.pow(self.scale as u32);
        let scale = self.scale as usize;
        write!(f, "{sign}{}.{:0scale$}", abs / pow, abs % pow)
    }
}

impl Eq for Numeric {}

impl From<Numeric> for f64 {
    fn from(n: Numeric) -> f64 {
        n.to_f64()
    }
}

impl From<Numeric> for i128 {
    fn from(n: Numeric) -> i128 {
        n.int_part()
    }
}

impl From<Numeric> for u128 {
    fn from(n: Numeric) -> u128 {
        n.int_part() as u128
    }
}

impl PartialEq for Numeric {
    fn eq(&self, other: &Self) -> bool {
        let (wide, narrow) = match self.scale.cmp(&other.scale) {
            Ordering::Less => (other, self),
            _ => (self, other),
        };

        // A rescale that overflows can't equal a value that fits in i128.
        10i128
            .checked_pow((wide.scale - narrow.scale) as u32)
            .and_then(|pow| pow.checked_mul(narrow.value))
            .is_some_and(|v| v == wide.value)
    }
}

#[cfg(feature = "rust_decimal")]
mod decimal {
    use super::{Decimal, Numeric};
    #[cfg(feature = "tds73")]
    use crate::{ColumnData, FromSql, FromSqlOwned};

    /// Past `Decimal`'s scale 28 or 96-bit mantissa, trailing zeros are dropped
    /// until it fits, as SqlClient's `SqlBuffer.Decimal` drops them rather than
    /// fail; any other loss is an error.
    #[cfg(feature = "tds73")]
    fn to_decimal(num: Numeric) -> crate::Result<Decimal> {
        let (mut value, mut scale) = (num.value(), num.scale() as u32);

        loop {
            if let Ok(dec) = Decimal::try_from_i128_with_scale(value, scale) {
                return Ok(dec);
            }
            if scale == 0 || value % 10 != 0 {
                return Err(crate::Error::Conversion(
                    format!("{num} does not fit in a rust_decimal::Decimal").into(),
                ));
            }
            value /= 10;
            scale -= 1;
        }
    }

    #[cfg(feature = "tds73")]
    impl<'a> FromSql<'a> for Decimal {
        fn from_sql(data: &'a ColumnData<'static>) -> crate::Result<Option<Self>> {
            match data {
                ColumnData::Numeric(num) => num.map(to_decimal).transpose(),
                _ => Err(crate::Error::Conversion(
                    format!("cannot interpret {:?} as an Decimal value", data).into(),
                )),
            }
        }
    }

    #[cfg(feature = "tds73")]
    impl FromSqlOwned for Decimal {
        fn from_sql_owned(data: ColumnData<'static>) -> crate::Result<Option<Self>> {
            Self::from_sql(&data)
        }
    }

    #[cfg(feature = "tds73")]
    to_sql!(self_,
            Decimal: (ColumnData::Numeric, {
                let unpacked = self_.unpack();

                let mut value = (((unpacked.hi as u128) << 64)
                                 + ((unpacked.mid as u128) << 32)
                                 + unpacked.lo as u128) as i128;

                if self_.is_sign_negative() {
                    value = -value;
                }

                Numeric::new_with_scale(value, self_.scale() as u8)
            });
    );

    #[cfg(feature = "tds73")]
    into_sql!(self_,
            Decimal: (ColumnData::Numeric, {
                let unpacked = self_.unpack();

                let mut value = (((unpacked.hi as u128) << 64)
                                 + ((unpacked.mid as u128) << 32)
                                 + unpacked.lo as u128) as i128;

                if self_.is_sign_negative() {
                    value = -value;
                }

                Numeric::new_with_scale(value, self_.scale() as u8)
            });
    );
}

#[cfg(feature = "bigdecimal")]
mod bigdecimal_ {
    use super::{BigDecimal, BigInt, Numeric};
    use crate::ColumnData;
    use num_traits::{Signed, ToPrimitive, Zero};

    #[cfg(feature = "tds73")]
    from_sql!(BigDecimal: ColumnData::Numeric(ref num) => num.map(|num| {
        let int = BigInt::from(num.value());

        BigDecimal::new(int, num.scale() as i64)
    }));

    /// `ToSql` cannot fail, so a value `decimal(38, s)` cannot hold exactly
    /// becomes a 39-digit `Numeric`, which the encoder rejects. Rounding is
    /// left to the caller (`with_scale_round`).
    fn to_numeric(value: &BigDecimal) -> Numeric {
        // value = int * 10^-exp
        let (int, exp) = value.as_bigint_and_exponent();

        if int.is_zero() {
            return Numeric::new_with_scale(0, exp.clamp(0, 38) as u8);
        }

        let max = if int.is_negative() {
            -i128::MAX
        } else {
            i128::MAX
        };
        let out_of_range = || Numeric::new_with_scale(max, 0);

        let (int, scale) = if exp < 0 {
            // The server has no negative scale: 9e3 is sent as 9000. Checked
            // before expanding, as the exponent can be huge.
            if value.digits().saturating_add(exp.unsigned_abs()) > 38 {
                return out_of_range();
            }
            (int * BigInt::from(10u8).pow(exp.unsigned_abs() as u32), 0)
        } else if exp > 38 {
            // Exact only if the digits past scale 38 are trailing zeros.
            let excess = exp - 38;
            if excess.unsigned_abs() >= value.digits() {
                return out_of_range();
            }
            let divisor = BigInt::from(10u8).pow(excess as u32);
            if !(&int % &divisor).is_zero() {
                return out_of_range();
            }
            (int / divisor, 38)
        } else {
            (int, exp as u8)
        };

        match int.to_i128() {
            Some(v) if v.unsigned_abs() < 10u128.pow(38) => Numeric::new_with_scale(v, scale),
            _ => out_of_range(),
        }
    }

    #[cfg(feature = "tds73")]
    to_sql!(self_,
            BigDecimal: (ColumnData::Numeric, to_numeric(self_));
    );

    #[cfg(feature = "tds73")]
    into_sql!(self_,
            BigDecimal: (ColumnData::Numeric, to_numeric(&self_));
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_eq() {
        assert_eq!(
            Numeric {
                value: 100501,
                scale: 2
            },
            Numeric {
                value: 1005010,
                scale: 3
            }
        );
        assert!(
            Numeric {
                value: 100501,
                scale: 2
            } != Numeric {
                value: 10050,
                scale: 1
            }
        );
    }

    #[test]
    fn numeric_eq_normalizes_across_a_scale_gap() {
        // 1.23 at scale 5 (123000) equals 1.23 at scale 2 (123). A scale gap of
        // 3 is chosen so the `self.scale - other.scale` exponent (3) differs from
        // both `+` (7) and `/` (1) — pinning the subtraction — and the
        // `10^gap * v` multiply differs from `+`/`/`. Both comparison directions
        // exercise the Greater and Less arms.
        let wide = Numeric {
            value: 123_000,
            scale: 5,
        };
        let narrow = Numeric {
            value: 123,
            scale: 2,
        };
        assert_eq!(wide, narrow); // Greater arm (self.scale > other.scale)
        assert_eq!(narrow, wide); // Less arm
        assert!(
            narrow
                != Numeric {
                    value: 124,
                    scale: 2
                }
        );
    }

    #[test]
    fn encode_byte_layout_matches_length_bucket() {
        // The encoder writes 1 length byte + 1 sign byte + (len-1) magnitude
        // bytes. This pins the per-length arms (deleting the 9- or 13-byte arm
        // would change the byte count) and the sign byte for zero.
        for value in [1i128, 10i128.pow(12), 10i128.pow(20), 10i128.pow(30)] {
            let n = Numeric::new_with_scale(value, 0);
            let expected = n.len() as usize + 1;
            let mut buf = BytesMut::new();
            n.encode(&mut buf).unwrap();
            assert_eq!(buf.len(), expected, "byte count for {value}");
        }

        // Zero is encoded as positive (sign byte 1), not negative.
        let mut zero = BytesMut::new();
        Numeric::new_with_scale(0, 0).encode(&mut zero).unwrap();
        assert_eq!(zero[1], 1, "zero must carry the positive sign byte");
    }

    #[tokio::test]
    async fn decode_d128_keeps_high_and_low_words() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

        // A magnitude whose high bytes are all non-zero: if decode_d128 wrongly
        // short-circuited on "all high bytes non-zero" it would drop the high
        // word and mis-decode. Positive (high byte 0x01 < i128::MAX high bit).
        let value = 0x0101_0101_0101_0101_0101_0101_0101_0101i128;
        let n = Numeric::new_with_scale(value, 0);
        let mut buf = BytesMut::new();
        n.encode(&mut buf).unwrap();
        let decoded = Numeric::decode(&mut buf.into_sql_read_bytes(), 0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(decoded.value(), value);
    }

    #[tokio::test]
    async fn decode_accepts_magnitude_at_i128_max_but_rejects_beyond() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

        // 17-byte form: len, sign(1 = positive), then 16 magnitude bytes.
        let mut at_max = BytesMut::new();
        at_max.put_u8(17);
        at_max.put_u8(1);
        at_max.put_i128_le(i128::MAX); // magnitude exactly i128::MAX
        let decoded = Numeric::decode(&mut at_max.into_sql_read_bytes(), 0)
            .await
            .expect("i128::MAX magnitude is representable")
            .unwrap();
        assert_eq!(decoded.value(), i128::MAX);

        // One past i128::MAX (high bit set) must be rejected, not wrapped.
        let mut beyond = BytesMut::new();
        beyond.put_u8(17);
        beyond.put_u8(1);
        beyond.put_u128_le((i128::MAX as u128) + 1);
        assert!(Numeric::decode(&mut beyond.into_sql_read_bytes(), 0)
            .await
            .is_err());
    }

    #[test]
    fn numeric_to_f64() {
        assert_eq!(f64::from(Numeric::new_with_scale(57705, 2)), 577.05);
    }

    #[test]
    fn numeric_to_int_dec_part() {
        let n = Numeric::new_with_scale(57705, 2);
        assert_eq!(n.int_part(), 577);
        assert_eq!(n.dec_part(), 5);
    }

    #[test]
    fn numeric_to_string() {
        assert_eq!(Numeric::new_with_scale(123, 0).to_string(), "123");
        assert_eq!(Numeric::new_with_scale(123, 1).to_string(), "12.3");
        assert_eq!(Numeric::new_with_scale(123, 2).to_string(), "1.23");
        assert_eq!(Numeric::new_with_scale(123, 3).to_string(), "0.123");
        assert_eq!(Numeric::new_with_scale(123, 4).to_string(), "0.0123");
        assert_eq!(
            Numeric::new_with_scale(123, 36).to_string(),
            "0.000000000000000000000000000000000123"
        );
        assert_eq!(
            Numeric::new_with_scale(123, 37).to_string(),
            "0.0000000000000000000000000000000000123"
        );
        assert_eq!(Numeric::new_with_scale(-123, 0).to_string(), "-123");
        assert_eq!(
            Numeric::new_with_scale(i128::MIN, 0).to_string(),
            "-170141183460469231731687303715884105728"
        );
        assert_eq!(Numeric::new_with_scale(-123, 1).to_string(), "-12.3");
        assert_eq!(Numeric::new_with_scale(-123, 2).to_string(), "-1.23");
        assert_eq!(Numeric::new_with_scale(-123, 3).to_string(), "-0.123");
        assert_eq!(Numeric::new_with_scale(-123, 4).to_string(), "-0.0123");
        assert_eq!(
            Numeric::new_with_scale(-123, 36).to_string(),
            "-0.000000000000000000000000000000000123"
        );
        assert_eq!(
            Numeric::new_with_scale(-123, 37).to_string(),
            "-0.0000000000000000000000000000000000123"
        );
    }

    #[test]
    fn calculates_precision_correctly() {
        let n = Numeric::new_with_scale(57705, 2);
        assert_eq!(5, n.precision());
    }

    #[test]
    fn new_with_scale_accessors() {
        let n = Numeric::new_with_scale(12345, 3);
        assert_eq!(n.value(), 12345);
        assert_eq!(n.scale(), 3);
        assert_eq!(n.int_part(), 12);
        assert_eq!(n.dec_part(), 345);
    }

    #[test]
    fn new_with_scale_allows_max_scale() {
        // decimal(38, 38) is valid in SQL Server, so scale 38 must be accepted.
        assert_eq!(Numeric::new_with_scale(1, 38).scale(), 38);
    }

    #[test]
    #[should_panic]
    fn new_with_scale_panics_on_too_large_scale() {
        Numeric::new_with_scale(1, 39);
    }

    #[test]
    fn precision_with_zero_int_part() {
        // 0.05 fits decimal(2, 2); no digit is spent on the leading zero.
        let n = Numeric::new_with_scale(5, 2);
        assert_eq!(n.int_part(), 0);
        assert_eq!(n.precision(), 2);

        assert_eq!(Numeric::new_with_scale(0, 0).precision(), 1);
        assert_eq!(Numeric::new_with_scale(0, 3).precision(), 3);
        assert_eq!(Numeric::new_with_scale(-5, 1).precision(), 1);
    }

    #[test]
    fn precision_of_decimal_38_38_is_38() {
        let half = Numeric::new_with_scale(5 * 10i128.pow(37), 38);
        assert_eq!(half.precision(), 38);
        let max = Numeric::new_with_scale(-(10i128.pow(38) - 1), 38);
        assert_eq!(max.precision(), 38);
        assert_eq!(max.len(), 17);
        assert_eq!(
            Numeric::new_with_scale(10i128.pow(38) - 1, 0).precision(),
            38
        );
    }

    #[test]
    fn eq_does_not_overflow_on_a_wide_scale_gap() {
        let big = Numeric::new_with_scale(10i128.pow(37), 0);
        let tiny = Numeric::new_with_scale(1, 38);
        assert_ne!(big, tiny);
        assert_ne!(tiny, big);
        assert_ne!(
            Numeric::new_with_scale(i128::MAX, 38),
            Numeric::new_with_scale(i128::MAX, 0)
        );
        assert_eq!(
            Numeric::new_with_scale(0, 0),
            Numeric::new_with_scale(0, 38)
        );
        assert_eq!(
            Numeric::new_with_scale(1, 0),
            Numeric::new_with_scale(10i128.pow(38), 38)
        );
    }

    #[test]
    fn to_float_is_correctly_rounded() {
        let money_max = Numeric::new_with_scale(i64::MAX as i128, 4);
        let nearest: f64 = "922337203685477.5807".parse().unwrap();
        assert_eq!(money_max.to_f64(), nearest);
        assert_eq!(f64::from(money_max), nearest);
        assert_eq!(Numeric::new_with_scale(12300, 4).to_f64(), 1.23);
        assert_eq!(Numeric::new_with_scale(-233, 2).to_f64(), -2.33);
        let tiny = Numeric::new_with_scale(123, 38);
        assert_eq!(tiny.to_f64(), 1.23e-36);

        assert_eq!(Numeric::new_with_scale(23300, 4).to_f32(), 2.33f32);
        assert_eq!(
            money_max.to_f32(),
            "922337203685477.5807".parse::<f32>().unwrap()
        );
    }

    #[tokio::test]
    async fn decode_rejects_a_scale_over_38() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

        let mut buf = BytesMut::new();
        buf.put_u8(5);
        buf.put_u8(1);
        buf.put_u32_le(1);
        let err = Numeric::decode(&mut buf.into_sql_read_bytes(), 39)
            .await
            .expect_err("scale 39 must error, not panic");
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[cfg(all(feature = "rust_decimal", feature = "tds73"))]
    #[test]
    fn rust_decimal_from_sql_errors_instead_of_panicking() {
        use crate::{ColumnData, FromSql, FromSqlOwned};

        let get = |value: i128, scale: u8| {
            let data = ColumnData::Numeric(Some(Numeric::new_with_scale(value, scale)));
            let borrowed = Decimal::from_sql(&data).map(|d| d.unwrap().to_string());
            let owned = Decimal::from_sql_owned(data).map(|d| d.unwrap().to_string());
            assert_eq!(borrowed.is_ok(), owned.is_ok());
            borrowed
        };

        // 38 digits, and scale 38 with significant digits past 28.
        assert!(get(12345678901234567890123456789012345678, 0).is_err());
        assert!(get(12345678901234567890123456789012345678, 38).is_err());
        assert!(matches!(
            get(10i128.pow(29), 0),
            Err(crate::Error::Conversion(_))
        ));

        // Only as many trailing zeros are dropped as needed to fit.
        assert_eq!(
            get(5 * 10i128.pow(29), 30).unwrap(),
            "0.5000000000000000000000000000"
        );
        assert_eq!(get(0, 38).unwrap(), "0.0000000000000000000000000000");
        assert_eq!(
            get(-(10i128.pow(31)), 3).unwrap(),
            "-10000000000000000000000000000"
        );
        assert_eq!(get(-12345, 2).unwrap(), "-123.45");
        assert_eq!(Decimal::from_sql(&ColumnData::Numeric(None)).unwrap(), None);
    }

    #[test]
    fn precision_scaling_by_length_buckets() {
        assert_eq!(Numeric::new_with_scale(1, 0).len(), 5);
        assert_eq!(Numeric::new_with_scale(1_000_000_000, 0).len(), 9);
        assert_eq!(Numeric::new_with_scale(10i128.pow(19), 0).len(), 13);
        assert_eq!(Numeric::new_with_scale(10i128.pow(28), 0).len(), 17);
    }

    #[test]
    fn display_and_debug() {
        let n = Numeric::new_with_scale(57705, 2);
        assert_eq!(format!("{:?}", n), "577.05");
        assert_eq!(format!("{}", n), "577.05");

        // Negative values format with a single leading sign and an unsigned
        // fractional part (see #390).
        let n = Numeric::new_with_scale(-57705, 3);
        assert_eq!(format!("{}", n), "-57.705");

        // Zero-padded fractional part for small decimals.
        let n = Numeric::new_with_scale(102, 4);
        assert_eq!(format!("{}", n), "0.0102");
    }

    #[test]
    fn from_numeric_conversions() {
        let n = Numeric::new_with_scale(57705, 2);
        assert_eq!(i128::from(n), 577);
        assert_eq!(u128::from(n), 577);
        assert!((f64::from(n) - 577.05).abs() < f64::EPSILON);
    }

    #[test]
    fn eq_across_scales_negative() {
        assert_eq!(
            Numeric::new_with_scale(-100501, 2),
            Numeric::new_with_scale(-1005010, 3),
        );
    }

    async fn round_trip(value: i128, scale: u8) {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

        let n = Numeric::new_with_scale(value, scale);
        let mut buf = BytesMut::new();
        n.encode(&mut buf).expect("encode must succeed");

        let decoded = Numeric::decode(&mut buf.into_sql_read_bytes(), scale)
            .await
            .expect("decode must succeed")
            .expect("value must be present");

        assert_eq!(decoded, n);
        assert_eq!(decoded.value(), value);
    }

    #[tokio::test]
    async fn encode_decode_round_trip() {
        round_trip(0, 0).await; // len 5
        round_trip(42, 0).await; // len 5
        round_trip(-42, 2).await; // negative, len 5
        round_trip(10i128.pow(12), 0).await; // len 9
        round_trip(10i128.pow(20), 0).await; // len 13
        round_trip(-(10i128.pow(20)), 3).await; // negative, len 13
        round_trip(10i128.pow(30), 0).await; // len 17
        round_trip(10i128.pow(38) - 1, 38).await; // 38 digits, the maximum
        round_trip(-(10i128.pow(38) - 1), 0).await;
    }

    #[test]
    fn encode_rejects_more_than_38_digits() {
        for value in [10i128.pow(38), -(10i128.pow(38)), i128::MAX, -i128::MAX] {
            let err = Numeric::new_with_scale(value, 0)
                .encode(&mut BytesMut::new())
                .unwrap_err();
            assert!(matches!(err, Error::Conversion(_)), "{value}: {err:?}");
        }
    }

    #[tokio::test]
    async fn decode_zero_length_is_none() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

        let mut buf = BytesMut::new();
        buf.put_u8(0);

        let decoded = Numeric::decode(&mut buf.into_sql_read_bytes(), 0)
            .await
            .expect("decode must succeed");

        assert!(decoded.is_none());
    }

    #[tokio::test]
    async fn decode_rejects_len17_magnitude_over_i128_max() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

        // len = 17, sign = 1 (positive), magnitude = 2^127 (byte[15] = 0x80),
        // which exceeds i128::MAX. Must return a protocol error rather than
        // wrapping to a negative value (or panicking on i128::MIN * -1).
        let mut buf = BytesMut::new();
        buf.put_u8(17);
        buf.put_u8(1);
        let mut mag = [0u8; 16];
        mag[15] = 0x80;
        buf.extend_from_slice(&mag);

        let err = Numeric::decode(&mut buf.into_sql_read_bytes(), 0)
            .await
            .expect_err("out-of-range magnitude must error");
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[tokio::test]
    async fn decode_rejects_invalid_sign_and_length() {
        use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;

        // Invalid sign byte (2 is neither 0 nor 1).
        let mut buf = BytesMut::new();
        buf.put_u8(5);
        buf.put_u8(2);
        buf.put_u32_le(1);
        let err = Numeric::decode(&mut buf.into_sql_read_bytes(), 0)
            .await
            .expect_err("invalid sign must error");
        assert!(matches!(err, Error::Protocol(_)));

        // Invalid length byte (6 is not one of 0/5/9/13/17).
        let mut buf = BytesMut::new();
        buf.put_u8(6);
        buf.put_u8(1);
        buf.extend_from_slice(&[0u8; 4]);
        let err = Numeric::decode(&mut buf.into_sql_read_bytes(), 0)
            .await
            .expect_err("invalid length must error");
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[cfg(all(feature = "bigdecimal", feature = "tds73"))]
    fn bigdecimal_to_numeric(value: &BigDecimal) -> Numeric {
        use crate::{ColumnData, IntoSql, ToSql};

        let by_ref = match value.to_sql() {
            ColumnData::Numeric(Some(n)) => n,
            other => panic!("{other:?}"),
        };
        let by_value = match value.clone().into_sql() {
            ColumnData::Numeric(Some(n)) => n,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            (by_ref.value(), by_ref.scale()),
            (by_value.value(), by_value.scale())
        );
        by_ref
    }

    #[test]
    #[cfg(all(feature = "bigdecimal", feature = "tds73"))]
    fn bigdecimal_out_of_range_is_an_encode_error() {
        use std::str::FromStr;

        let mut values = vec![BigDecimal::from(1) / BigDecimal::from(3)];
        for s in [
            "1e40",
            "-1e40",
            "1e38",
            "1e-39",
            "-1e-39",
            "0.1234567890123456789012345678901234567890",
            "1e-1000000000",
            "1e1000000000",
            "1e-9223372036854775807",
        ] {
            values.push(BigDecimal::from_str(s).unwrap());
        }

        for value in values {
            let err = bigdecimal_to_numeric(&value)
                .encode(&mut BytesMut::new())
                .unwrap_err();
            assert!(matches!(err, Error::Conversion(_)), "{value}: {err:?}");
        }
    }

    #[test]
    #[cfg(all(feature = "bigdecimal", feature = "tds73"))]
    fn bigdecimal_exact_values_keep_value_and_scale() {
        use std::str::FromStr;

        let nines = |n: usize| "9".repeat(n);
        let cases = [
            ("1.5e3".to_string(), 1500, 0),
            ("1e-3".to_string(), 1, 3),
            ("-12.345".to_string(), -12345, 3),
            (nines(38), 10i128.pow(38) - 1, 0),
            (format!("0.{}", nines(38)), 10i128.pow(38) - 1, 38),
            (format!("-{}e-38", nines(38)), -(10i128.pow(38) - 1), 38),
            // Scale 41, but the three digits past 38 are zeros.
            (format!("0.15{}", "0".repeat(39)), 15 * 10i128.pow(36), 38),
            ("0e-50".to_string(), 0, 38),
            ("0e50".to_string(), 0, 0),
        ];

        for (s, value, scale) in cases {
            let n = bigdecimal_to_numeric(&BigDecimal::from_str(&s).unwrap());
            assert_eq!((n.value(), n.scale()), (value, scale), "{s}");
            n.encode(&mut BytesMut::new()).unwrap();
        }
    }

    #[test]
    #[cfg(feature = "bigdecimal")]
    fn no_overflowing_pow() {
        use crate::{ColumnData, ToSql};
        use bigdecimal::FromPrimitive;

        let dec = BigDecimal::new(BigInt::from_i8(1).unwrap(), -20);
        let res = dec.to_sql();

        assert_eq!(
            ColumnData::Numeric(Some(Numeric::new_with_scale(100000000000000000000i128, 0))),
            res
        );
    }
}
