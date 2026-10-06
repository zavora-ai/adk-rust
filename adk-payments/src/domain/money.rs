use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Largest decimal scale that [`Money`] parsing and rescaling accept.
///
/// `10^18` is the largest power of ten that fits in `i64`, so a larger scale
/// cannot represent even one major unit.
pub const MAX_MONEY_SCALE: u32 = 18;

/// ISO 4217 codes whose minor unit has no decimal places.
const ZERO_DECIMAL_CURRENCIES: &[&str] = &[
    "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "UYI", "VND",
    "VUV", "XAF", "XOF", "XPF",
];

/// ISO 4217 codes whose minor unit has three decimal places.
const THREE_DECIMAL_CURRENCIES: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];

/// ISO 4217 codes whose minor unit has four decimal places.
const FOUR_DECIMAL_CURRENCIES: &[&str] = &["CLF", "UYW"];

/// ISO 4217 codes that define no minor unit (precious metals, funds, testing).
const NO_MINOR_UNIT_CURRENCIES: &[&str] =
    &["XAG", "XAU", "XBA", "XBB", "XBC", "XBD", "XDR", "XPD", "XPT", "XSU", "XTS", "XUA", "XXX"];

/// Errors raised by exact [`Money`] parsing and arithmetic.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MoneyError {
    #[error(
        "`{value}` is not a valid decimal monetary value: {reason}. Send a plain decimal number such as `12.34`."
    )]
    InvalidDecimal { value: String, reason: String },

    #[error(
        "`{value}` requires {scale} fraction digits, more than the allowed {max_scale}. Round the amount to at most {max_scale} decimal places."
    )]
    PrecisionTooHigh { value: String, scale: u32, max_scale: u32 },

    #[error(
        "monetary arithmetic overflowed while computing {context}. Reduce the amount or its precision."
    )]
    Overflow { context: String },

    #[error(
        "currency `{found}` does not match `{expected}`. Express every amount in one cart or mandate in the same currency."
    )]
    CurrencyMismatch { expected: String, found: String },
}

/// Currency-denominated amount stored as minor units plus an explicit scale.
///
/// `amount_minor` keeps commerce arithmetic exact and avoids floating-point
/// rounding drift. The value is `amount_minor / 10^scale` major units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    pub currency: String,
    pub amount_minor: i64,
    pub scale: u32,
}

impl Money {
    /// Creates a money value using explicit minor units and scale.
    #[must_use]
    pub fn new(currency: impl Into<String>, amount_minor: i64, scale: u32) -> Self {
        Self { currency: currency.into(), amount_minor, scale }
    }

    /// Returns the ISO 4217 minor-unit exponent for `currency`.
    ///
    /// The lookup ignores ASCII case. Codes outside the zero-, three-, and
    /// four-decimal ISO 4217 lists are treated as two-decimal currencies.
    /// Returns `None` for codes that define no minor unit (such as `XXX` or
    /// `XAU`) and for strings that are not three ASCII letters.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::domain::Money;
    ///
    /// assert_eq!(Money::iso_minor_unit_scale("usd"), Some(2));
    /// assert_eq!(Money::iso_minor_unit_scale("JPY"), Some(0));
    /// assert_eq!(Money::iso_minor_unit_scale("KWD"), Some(3));
    /// assert_eq!(Money::iso_minor_unit_scale("XXX"), None);
    /// ```
    #[must_use]
    pub fn iso_minor_unit_scale(currency: &str) -> Option<u32> {
        if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return None;
        }
        let code = currency.to_ascii_uppercase();
        let code = code.as_str();
        if NO_MINOR_UNIT_CURRENCIES.contains(&code) {
            None
        } else if ZERO_DECIMAL_CURRENCIES.contains(&code) {
            Some(0)
        } else if THREE_DECIMAL_CURRENCIES.contains(&code) {
            Some(3)
        } else if FOUR_DECIMAL_CURRENCIES.contains(&code) {
            Some(4)
        } else {
            Some(2)
        }
    }

    /// Parses a decimal string such as `-12.50` or `1.5e-3` without rounding.
    ///
    /// The sign is read from the string itself, so `-0.50` parses to `-50`
    /// minor units. The resulting scale is the number of significant fraction
    /// digits: trailing fractional zeros are dropped and integers have scale 0.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::domain::Money;
    ///
    /// assert_eq!(Money::parse_decimal("USD", "-0.50").unwrap(), Money::new("USD", -5, 1));
    /// assert_eq!(Money::parse_decimal("USD", "1.125").unwrap(), Money::new("USD", 1_125, 3));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::InvalidDecimal`] for malformed input,
    /// [`MoneyError::PrecisionTooHigh`] when more than [`MAX_MONEY_SCALE`]
    /// significant fraction digits remain, and [`MoneyError::Overflow`] when
    /// the value does not fit in `i64` minor units.
    pub fn parse_decimal(currency: impl Into<String>, value: &str) -> Result<Self, MoneyError> {
        let invalid = |reason: &str| MoneyError::InvalidDecimal {
            value: value.to_string(),
            reason: reason.to_string(),
        };
        let (negative, unsigned) = match value.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
            Some(index) => (&unsigned[..index], Some(&unsigned[index + 1..])),
            None => (unsigned, None),
        };
        let (whole, fraction) = match mantissa.split_once('.') {
            Some((whole, "")) => {
                return Err(invalid(&format!("`{whole}.` has no digits after the decimal point")));
            }
            Some((whole, fraction)) => (whole, fraction),
            None => (mantissa, ""),
        };
        if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("the integer part must contain only ASCII digits"));
        }
        if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("the fraction part must contain only ASCII digits"));
        }
        let exponent = match exponent {
            None => 0_i64,
            Some(exponent) => {
                let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid("the exponent must be an optionally signed integer"));
                }
                exponent.parse::<i64>().map_err(|_| MoneyError::Overflow {
                    context: format!("the exponent of `{value}`"),
                })?
            }
        };

        let mut digits = format!("{whole}{fraction}");
        let significant_start = digits.find(|digit| digit != '0').unwrap_or(digits.len());
        digits.drain(..significant_start);
        if digits.is_empty() {
            return Ok(Self::new(currency, 0, 0));
        }

        let fraction_len = i64::try_from(fraction.len()).map_err(|_| MoneyError::Overflow {
            context: format!("the fraction length of `{value}`"),
        })?;
        let mut scale = fraction_len
            .checked_sub(exponent)
            .ok_or_else(|| MoneyError::Overflow { context: format!("the scale of `{value}`") })?;
        while scale > 0 && digits.ends_with('0') {
            digits.pop();
            scale -= 1;
        }
        if scale > i64::from(MAX_MONEY_SCALE) {
            return Err(MoneyError::PrecisionTooHigh {
                value: value.to_string(),
                scale: u32::try_from(scale).unwrap_or(u32::MAX),
                max_scale: MAX_MONEY_SCALE,
            });
        }

        let overflow = || MoneyError::Overflow { context: format!("the minor units of `{value}`") };
        let mut magnitude = digits.bytes().try_fold(0_i128, |acc, digit| {
            acc.checked_mul(10)?.checked_add(i128::from(digit - b'0'))
        });
        if scale < 0 {
            let factor = u32::try_from(-scale).ok().and_then(|power| 10_i128.checked_pow(power));
            magnitude = magnitude.zip(factor).and_then(|(value, factor)| value.checked_mul(factor));
            scale = 0;
        }
        let magnitude = magnitude.ok_or_else(overflow)?;
        let signed = if negative { -magnitude } else { magnitude };
        let amount_minor = i64::try_from(signed).map_err(|_| overflow())?;
        let scale = u32::try_from(scale).map_err(|_| overflow())?;
        Ok(Self::new(currency, amount_minor, scale))
    }

    /// Re-expresses this amount at `scale` without rounding.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::domain::Money;
    ///
    /// let amount = Money::new("USD", 25, 1);
    /// assert_eq!(amount.rescaled(3).unwrap(), Money::new("USD", 2_500, 3));
    /// assert_eq!(Money::new("USD", 2_500, 3).rescaled(2).unwrap(), Money::new("USD", 250, 2));
    /// assert!(Money::new("USD", 1_125, 3).rescaled(2).is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::PrecisionTooHigh`] when lowering the scale would
    /// drop non-zero digits or `scale` exceeds [`MAX_MONEY_SCALE`], and
    /// [`MoneyError::Overflow`] when the rescaled value does not fit in `i64`.
    pub fn rescaled(&self, scale: u32) -> Result<Self, MoneyError> {
        if scale > MAX_MONEY_SCALE {
            return Err(MoneyError::PrecisionTooHigh {
                value: self.to_decimal_string(),
                scale,
                max_scale: MAX_MONEY_SCALE,
            });
        }
        let overflow = || MoneyError::Overflow {
            context: format!("`{}` at scale {scale}", self.to_decimal_string()),
        };
        let amount_minor = match scale.cmp(&self.scale) {
            Ordering::Equal => self.amount_minor,
            Ordering::Greater => {
                let factor = 10_i64.checked_pow(scale - self.scale).ok_or_else(overflow)?;
                self.amount_minor.checked_mul(factor).ok_or_else(overflow)?
            }
            Ordering::Less => {
                let exact = 10_i64
                    .checked_pow(self.scale - scale)
                    .filter(|factor| self.amount_minor % factor == 0);
                match exact {
                    Some(factor) => self.amount_minor / factor,
                    None if self.amount_minor == 0 => 0,
                    None => {
                        return Err(MoneyError::PrecisionTooHigh {
                            value: self.to_decimal_string(),
                            scale: self.scale,
                            max_scale: scale,
                        });
                    }
                }
            }
        };
        Ok(Self::new(self.currency.clone(), amount_minor, scale))
    }

    /// Adds two amounts of the same currency, aligning them to the larger scale.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::domain::Money;
    ///
    /// let sum = Money::new("USD", 1_125, 3).checked_add(&Money::new("USD", 25, 1)).unwrap();
    /// assert_eq!(sum, Money::new("USD", 3_625, 3));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::CurrencyMismatch`] when the currencies differ
    /// (ignoring ASCII case) and [`MoneyError::Overflow`] when the sum does not
    /// fit in `i64` minor units.
    pub fn checked_add(&self, other: &Self) -> Result<Self, MoneyError> {
        let (left, right) = self.aligned_with(other)?;
        let amount_minor = left.amount_minor.checked_add(right.amount_minor).ok_or_else(|| {
            MoneyError::Overflow {
                context: format!(
                    "`{}` + `{}`",
                    self.to_decimal_string(),
                    other.to_decimal_string()
                ),
            }
        })?;
        Ok(Self::new(left.currency, amount_minor, left.scale))
    }

    /// Subtracts `other` from this amount, aligning both to the larger scale.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::domain::Money;
    ///
    /// let delta = Money::new("USD", 500, 2).checked_sub(&Money::new("USD", 1_125, 3)).unwrap();
    /// assert_eq!(delta, Money::new("USD", 3_875, 3));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::CurrencyMismatch`] when the currencies differ
    /// (ignoring ASCII case) and [`MoneyError::Overflow`] when the difference
    /// does not fit in `i64` minor units.
    pub fn checked_sub(&self, other: &Self) -> Result<Self, MoneyError> {
        let (left, right) = self.aligned_with(other)?;
        let amount_minor = left.amount_minor.checked_sub(right.amount_minor).ok_or_else(|| {
            MoneyError::Overflow {
                context: format!(
                    "`{}` - `{}`",
                    self.to_decimal_string(),
                    other.to_decimal_string()
                ),
            }
        })?;
        Ok(Self::new(left.currency, amount_minor, left.scale))
    }

    /// Compares two amounts numerically, independent of their scales.
    ///
    /// Returns `None` when the currencies differ (ignoring ASCII case) or the
    /// values cannot be aligned without overflow.
    ///
    /// # Example
    ///
    /// ```
    /// use std::cmp::Ordering;
    ///
    /// use adk_payments::domain::Money;
    ///
    /// let three_scale = Money::new("USD", 40_000, 3);
    /// let two_scale = Money::new("USD", 5_000, 2);
    /// assert_eq!(three_scale.compare_amount(&two_scale), Some(Ordering::Less));
    /// assert_eq!(three_scale.compare_amount(&Money::new("JPY", 1, 0)), None);
    /// ```
    #[must_use]
    pub fn compare_amount(&self, other: &Self) -> Option<Ordering> {
        if !self.currency.eq_ignore_ascii_case(&other.currency) {
            return None;
        }
        let scale = self.scale.max(other.scale);
        let widen = |money: &Self| {
            10_i128
                .checked_pow(scale - money.scale)
                .and_then(|factor| i128::from(money.amount_minor).checked_mul(factor))
        };
        Some(widen(self)?.cmp(&widen(other)?))
    }

    /// Renders the amount as a plain decimal string such as `-12.50`.
    ///
    /// Scales above [`MAX_MONEY_SCALE`] render in exponent form so a corrupt
    /// scale cannot force a large allocation.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::domain::Money;
    ///
    /// assert_eq!(Money::new("USD", -1_250, 2).to_decimal_string(), "-12.50");
    /// assert_eq!(Money::new("JPY", 1_200, 0).to_decimal_string(), "1200");
    /// ```
    #[must_use]
    pub fn to_decimal_string(&self) -> String {
        let sign = if self.amount_minor < 0 { "-" } else { "" };
        let magnitude = self.amount_minor.unsigned_abs();
        if self.scale == 0 {
            return format!("{sign}{magnitude}");
        }
        if self.scale > MAX_MONEY_SCALE {
            return format!("{sign}{magnitude}e-{}", self.scale);
        }
        let width = usize::try_from(self.scale).unwrap_or(usize::MAX).saturating_add(1);
        let digits = format!("{magnitude:0>width$}");
        let (whole, fraction) = digits.split_at(digits.len() - (width - 1));
        format!("{sign}{whole}.{fraction}")
    }

    fn aligned_with(&self, other: &Self) -> Result<(Self, Self), MoneyError> {
        if !self.currency.eq_ignore_ascii_case(&other.currency) {
            return Err(MoneyError::CurrencyMismatch {
                expected: self.currency.clone(),
                found: other.currency.clone(),
            });
        }
        let scale = self.scale.max(other.scale);
        Ok((self.rescaled(scale)?, other.rescaled(scale)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_fraction_below_one_keeps_its_sign() {
        assert_eq!(Money::parse_decimal("USD", "-0.50").unwrap(), Money::new("USD", -5, 1));
        assert_eq!(Money::parse_decimal("USD", "-0").unwrap(), Money::new("USD", 0, 0));
    }

    #[test]
    fn parses_exponent_notation_exactly() {
        assert_eq!(Money::parse_decimal("USD", "1.5e-7").unwrap(), Money::new("USD", 15, 8));
        assert_eq!(
            Money::parse_decimal("USD", "1e21").unwrap_err(),
            MoneyError::Overflow { context: "the minor units of `1e21`".to_string() }
        );
        assert_eq!(Money::parse_decimal("USD", "12E+2").unwrap(), Money::new("USD", 1_200, 0));
    }

    #[test]
    fn rejects_more_than_eighteen_significant_fraction_digits() {
        assert_eq!(
            Money::parse_decimal("USD", "0.0000000000000000001").unwrap_err(),
            MoneyError::PrecisionTooHigh {
                value: "0.0000000000000000001".to_string(),
                scale: 19,
                max_scale: MAX_MONEY_SCALE,
            }
        );
        assert_eq!(
            Money::parse_decimal("USD", "1.000000000000000000000").unwrap(),
            Money::new("USD", 1, 0)
        );
    }

    #[test]
    fn rejects_malformed_decimals() {
        for value in ["", "-", "1.", ".5", "1.2.3", "1e", "abc", "1_000", "+1", "1e+"] {
            assert!(
                matches!(
                    Money::parse_decimal("USD", value),
                    Err(MoneyError::InvalidDecimal { .. })
                ),
                "`{value}` should be rejected"
            );
        }
    }

    #[test]
    fn rescaling_overflow_is_an_error_not_a_panic() {
        assert_eq!(
            Money::new("USD", i64::MAX, 0).rescaled(2).unwrap_err(),
            MoneyError::Overflow { context: format!("`{}` at scale 2", i64::MAX) }
        );
        assert!(Money::new("USD", 1, 0).rescaled(19).is_err());
    }

    #[test]
    fn checked_add_aligns_scales_and_rejects_other_currencies() {
        assert_eq!(
            Money::new("USD", 1_125, 3).checked_add(&Money::new("usd", 25, 1)).unwrap(),
            Money::new("USD", 3_625, 3)
        );
        assert_eq!(
            Money::new("USD", 1, 2).checked_add(&Money::new("EUR", 1, 2)).unwrap_err(),
            MoneyError::CurrencyMismatch { expected: "USD".to_string(), found: "EUR".to_string() }
        );
    }

    #[test]
    fn formats_minimum_value_without_overflow() {
        assert_eq!(Money::new("USD", i64::MIN, 2).to_decimal_string(), "-92233720368547758.08");
    }
}
