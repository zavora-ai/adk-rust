use std::cmp::Ordering;

use adk_payments::domain::{MAX_MONEY_SCALE, Money, MoneyError};
use proptest::prelude::*;

fn scale() -> impl Strategy<Value = u32> {
    0_u32..=MAX_MONEY_SCALE
}

/// Reference value of `amount_minor / 10^scale` aligned to `target` in `i128`.
fn widened(amount_minor: i64, scale: u32, target: u32) -> i128 {
    i128::from(amount_minor) * 10_i128.pow(target - scale)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn decimal_string_round_trips_through_parse(amount_minor in any::<i64>(), scale in scale()) {
        let money = Money::new("USD", amount_minor, scale);
        let parsed = Money::parse_decimal("USD", &money.to_decimal_string()).unwrap();

        prop_assert!(parsed.scale <= scale);
        prop_assert_eq!(parsed.compare_amount(&money), Some(Ordering::Equal));
        prop_assert_eq!(parsed.rescaled(scale).unwrap(), money);
    }

    #[test]
    fn exponent_form_parses_to_the_same_value(
        amount_minor in -1_000_000_000_i64..1_000_000_000,
        scale in 0_u32..=9,
    ) {
        let money = Money::new("USD", amount_minor, scale);
        let exponent_form = format!("{amount_minor}e-{scale}");
        let parsed = Money::parse_decimal("USD", &exponent_form).unwrap();

        prop_assert_eq!(parsed.compare_amount(&money), Some(Ordering::Equal));
    }

    #[test]
    fn parsing_arbitrary_text_never_panics(text in ".{0,48}") {
        let _ = Money::parse_decimal("USD", &text);
    }

    #[test]
    fn parsing_numeric_looking_text_never_panics(
        text in "-?[0-9]{0,40}(\\.[0-9]{0,40})?([eE][+-]?[0-9]{0,25})?"
    ) {
        if let Ok(money) = Money::parse_decimal("USD", &text) {
            prop_assert!(money.scale <= MAX_MONEY_SCALE);
        }
    }

    #[test]
    fn checked_add_matches_exact_reference(
        left in any::<i64>(),
        left_scale in 0_u32..=6,
        right in any::<i64>(),
        right_scale in 0_u32..=6,
    ) {
        let target = left_scale.max(right_scale);
        let expected = widened(left, left_scale, target) + widened(right, right_scale, target);
        let result = Money::new("USD", left, left_scale)
            .checked_add(&Money::new("usd", right, right_scale));

        match i64::try_from(expected) {
            Ok(expected) if i64::try_from(widened(left, left_scale, target)).is_ok()
                && i64::try_from(widened(right, right_scale, target)).is_ok() =>
            {
                prop_assert_eq!(result.unwrap(), Money::new("USD", expected, target));
            }
            Ok(_) | Err(_) => {
                let overflowed = matches!(result, Err(MoneyError::Overflow { .. }));
                prop_assert!(overflowed, "expected an overflow error, got {:?}", result);
            }
        }
    }

    #[test]
    fn checked_sub_inverts_checked_add(
        left in -1_000_000_000_000_i64..1_000_000_000_000,
        left_scale in 0_u32..=6,
        right in -1_000_000_000_000_i64..1_000_000_000_000,
        right_scale in 0_u32..=6,
    ) {
        let left = Money::new("USD", left, left_scale);
        let right = Money::new("USD", right, right_scale);
        let sum = left.checked_add(&right).unwrap();

        prop_assert_eq!(sum.checked_sub(&right).unwrap().compare_amount(&left), Some(Ordering::Equal));
    }

    #[test]
    fn compare_amount_matches_exact_reference(
        left in any::<i64>(),
        left_scale in scale(),
        right in any::<i64>(),
        right_scale in scale(),
    ) {
        let target = left_scale.max(right_scale);
        let expected = widened(left, left_scale, target).cmp(&widened(right, right_scale, target));

        prop_assert_eq!(
            Money::new("USD", left, left_scale).compare_amount(&Money::new("USD", right, right_scale)),
            Some(expected)
        );
    }

    #[test]
    fn rescaling_up_and_back_is_lossless(
        amount_minor in -1_000_000_000_i64..1_000_000_000,
        from in 0_u32..=6,
        extra in 0_u32..=6,
    ) {
        let money = Money::new("USD", amount_minor, from);
        let widened = money.rescaled(from + extra).unwrap();

        prop_assert_eq!(widened.rescaled(from).unwrap(), money);
    }
}

#[cfg(feature = "ap2")]
mod ap2 {
    use adk_payments::domain::Money;
    use adk_payments::protocol::ap2::PaymentCurrencyAmount;
    use proptest::prelude::*;
    use serde_json::json;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Values with at most 15 significant digits survive the JSON `f64` exactly.
        #[test]
        fn ap2_amount_converts_without_rounding_or_sign_loss(
            amount_minor in -999_999_999_999_i64..999_999_999_999,
            scale in 0_u32..=3,
            currency in prop::sample::select(vec!["USD", "JPY", "KWD"]),
        ) {
            let decimal = Money::new(currency, amount_minor, scale).to_decimal_string();
            let value: serde_json::Value = serde_json::from_str(&decimal).unwrap();
            let amount: PaymentCurrencyAmount =
                serde_json::from_value(json!({"currency": currency, "value": value})).unwrap();

            let money = amount.to_money().unwrap();
            let exact = Money::parse_decimal(currency, &decimal).unwrap();
            let currency_scale = Money::iso_minor_unit_scale(currency).unwrap();

            prop_assert_eq!(money.scale, currency_scale.max(exact.scale));
            prop_assert_eq!(money.compare_amount(&exact), Some(std::cmp::Ordering::Equal));
            prop_assert_eq!(money.amount_minor.signum(), amount_minor.signum());
        }
    }
}
