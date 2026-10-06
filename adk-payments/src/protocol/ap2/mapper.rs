use serde_json::Value;

use crate::AP2_ALPHA_BASELINE;
use crate::domain::{
    Cart, CartLine, FulfillmentKind, FulfillmentSelection, Money, MoneyError, OrderSnapshot,
    OrderState, PaymentMethodSelection, PriceAdjustment, PriceAdjustmentKind, ProtocolDescriptor,
    ProtocolExtensionEnvelope, ProtocolExtensions, ReceiptState, TransactionRecord,
    TransactionState,
};
use crate::kernel::{
    CommerceContext, CreateCheckoutCommand, ExecutePaymentCommand, PaymentExecutionOutcome,
    SyncPaymentOutcomeCommand,
};
use crate::protocol::ap2::error::Ap2Error;
use crate::protocol::ap2::types::{
    CartMandate, IntentMandate, PaymentCurrencyAmount, PaymentMandate, PaymentReceipt,
    PaymentStatusEnvelope,
};

pub(crate) fn ap2_descriptor() -> ProtocolDescriptor {
    ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE)
}

pub(crate) fn merge_extensions(
    mut left: ProtocolExtensions,
    right: ProtocolExtensions,
) -> ProtocolExtensions {
    for envelope in right.0 {
        left.push(envelope);
    }
    left
}

pub(crate) fn placeholder_cart_from_intent(intent: &IntentMandate) -> Cart {
    Cart {
        cart_id: None,
        lines: vec![CartLine {
            line_id: "intent".to_string(),
            merchant_sku: intent.skus.as_ref().and_then(|skus| skus.first().cloned()),
            title: "intent authorization".to_string(),
            quantity: 1,
            unit_price: Money::new("XXX", 0, 2),
            total_price: Money::new("XXX", 0, 2),
            product_class: Some("intent".to_string()),
            extensions: ProtocolExtensions::default(),
        }],
        subtotal: Some(Money::new("XXX", 0, 2)),
        adjustments: Vec::new(),
        total: Money::new("XXX", 0, 2),
        affiliate_attribution: None,
        extensions: ProtocolExtensions::default(),
    }
}

/// Every amount that contributes to the canonical cart built from `mandate`.
fn cart_amounts(mandate: &CartMandate) -> impl Iterator<Item = &PaymentCurrencyAmount> {
    let details = &mandate.contents.payment_request.details;
    let selected_shipping = details
        .shipping_options
        .iter()
        .flatten()
        .filter(|option| option.selected)
        .map(|option| &option.amount);
    let modifier_items = details
        .modifiers
        .iter()
        .flatten()
        .flat_map(|modifier| modifier.additional_display_items.iter().flatten())
        .map(|item| &item.amount);
    details
        .display_items
        .iter()
        .map(|item| &item.amount)
        .chain(std::iter::once(&details.total.amount))
        .chain(selected_shipping)
        .chain(modifier_items)
}

/// Returns the single scale every amount in `mandate` is normalised to.
///
/// The scale is the cart currency's ISO 4217 minor-unit scale, raised to the
/// most precise amount in the cart so that no amount is rounded. Currencies
/// without a minor unit start from scale 0.
pub(crate) fn cart_amount_scale(mandate: &CartMandate) -> Result<u32, Ap2Error> {
    let currency = &mandate.contents.payment_request.details.total.amount.currency;
    let mut scale = Money::iso_minor_unit_scale(currency).unwrap_or(0);
    for amount in cart_amounts(mandate) {
        if !amount.currency.eq_ignore_ascii_case(currency) {
            return Err(MoneyError::CurrencyMismatch {
                expected: currency.clone(),
                found: amount.currency.clone(),
            }
            .into());
        }
        scale = scale.max(amount.to_exact_money()?.scale);
    }
    Ok(scale)
}

fn money_at_scale(amount: &PaymentCurrencyAmount, scale: u32) -> Result<Money, Ap2Error> {
    Ok(amount.to_exact_money()?.rescaled(scale)?)
}

pub(crate) fn cart_from_cart_mandate(mandate: &CartMandate) -> Result<Cart, Ap2Error> {
    let scale = cart_amount_scale(mandate)?;
    let details = &mandate.contents.payment_request.details;
    let total = money_at_scale(&details.total.amount, scale)?;
    let mut lines = Vec::with_capacity(details.display_items.len());
    let mut subtotal = Money::new(total.currency.clone(), 0, scale);

    for (index, item) in details.display_items.iter().enumerate() {
        let line_total = money_at_scale(&item.amount, scale)?;
        subtotal = subtotal.checked_add(&line_total)?;
        lines.push(CartLine {
            line_id: format!("{}:{index}", mandate.contents.id),
            merchant_sku: None,
            title: item.label.clone(),
            quantity: 1,
            unit_price: line_total.clone(),
            total_price: line_total,
            product_class: None,
            extensions: ProtocolExtensions::default(),
        });
    }

    let mut adjustments = Vec::new();
    if let Some(options) = &details.shipping_options {
        for option in options.iter().filter(|option| option.selected) {
            adjustments.push(PriceAdjustment {
                adjustment_id: option.id.clone(),
                kind: PriceAdjustmentKind::Shipping,
                label: option.label.clone(),
                amount: money_at_scale(&option.amount, scale)?,
                extensions: ProtocolExtensions::default(),
            });
        }
    }

    if let Some(modifiers) = &details.modifiers {
        for modifier in modifiers {
            if let Some(items) = &modifier.additional_display_items {
                for (index, item) in items.iter().enumerate() {
                    adjustments.push(PriceAdjustment {
                        adjustment_id: format!("{}:{index}", modifier.supported_methods),
                        kind: PriceAdjustmentKind::Fee,
                        label: item.label.clone(),
                        amount: money_at_scale(&item.amount, scale)?,
                        extensions: ProtocolExtensions::default(),
                    });
                }
            }
        }
    }

    let mut allocated = subtotal.clone();
    for adjustment in &adjustments {
        allocated = allocated.checked_add(&adjustment.amount)?;
    }
    let unallocated = total.checked_sub(&allocated)?;
    if unallocated.amount_minor != 0 {
        adjustments.push(PriceAdjustment {
            adjustment_id: "ap2_unallocated_delta".to_string(),
            kind: PriceAdjustmentKind::Other("ap2".to_string()),
            label: "AP2 total reconciliation".to_string(),
            amount: unallocated,
            extensions: ProtocolExtensions::default(),
        });
    }

    Ok(Cart {
        cart_id: Some(mandate.contents.id.clone()),
        lines,
        subtotal: Some(subtotal),
        adjustments,
        total,
        affiliate_attribution: None,
        extensions: ProtocolExtensions::default(),
    })
}

pub(crate) fn fulfillment_from_cart_mandate(
    mandate: &CartMandate,
) -> Result<Option<FulfillmentSelection>, Ap2Error> {
    let Some(option) = mandate
        .contents
        .payment_request
        .details
        .shipping_options
        .as_ref()
        .and_then(|options| options.iter().find(|option| option.selected))
    else {
        return Ok(None);
    };
    Ok(Some(FulfillmentSelection {
        fulfillment_id: option.id.clone(),
        kind: FulfillmentKind::Shipping,
        label: option.label.clone(),
        amount: Some(money_at_scale(&option.amount, cart_amount_scale(mandate)?)?),
        destination: None,
        requires_user_selection: mandate
            .contents
            .payment_request
            .options
            .as_ref()
            .is_some_and(|options| options.request_shipping),
        extensions: ProtocolExtensions::default(),
    }))
}

pub(crate) fn intent_create_checkout_command(
    intent: &IntentMandate,
    context: CommerceContext,
) -> CreateCheckoutCommand {
    CreateCheckoutCommand { context, cart: placeholder_cart_from_intent(intent), fulfillment: None }
}

pub(crate) fn payment_method_selection(mandate: &PaymentMandate) -> PaymentMethodSelection {
    let reference = mandate
        .payment_mandate_contents
        .payment_response
        .details
        .as_ref()
        .and_then(|details| details.get("token"))
        .and_then(Value::as_object)
        .and_then(|token| token.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    PaymentMethodSelection {
        selection_kind: mandate.payment_mandate_contents.payment_response.method_name.clone(),
        reference,
        display_hint: None,
        extensions: ProtocolExtensions::default(),
    }
}

/// Builds the payment command, expressing the amount at the cart's `scale`.
pub(crate) fn execute_payment_command(
    mandate: &PaymentMandate,
    scale: u32,
    context: CommerceContext,
    supporting_evidence_refs: Vec<crate::domain::EvidenceReference>,
) -> Result<ExecutePaymentCommand, Ap2Error> {
    Ok(ExecutePaymentCommand {
        context,
        amount: money_at_scale(
            &mandate.payment_mandate_contents.payment_details_total.amount,
            scale,
        )?,
        selected_payment_method: Some(payment_method_selection(mandate)),
        supporting_evidence_refs,
        extensions: ProtocolExtensions::default(),
    })
}

pub(crate) fn sync_payment_outcome_command(
    record: Option<&TransactionRecord>,
    receipt: &PaymentReceipt,
    context: CommerceContext,
) -> SyncPaymentOutcomeCommand {
    let outcome = match receipt.payment_status {
        PaymentStatusEnvelope::Success(_) => PaymentExecutionOutcome::Completed,
        PaymentStatusEnvelope::Error(_) | PaymentStatusEnvelope::Failure(_) => {
            PaymentExecutionOutcome::Failed
        }
    };
    let order_state = match outcome {
        PaymentExecutionOutcome::Completed => OrderState::Completed,
        PaymentExecutionOutcome::Failed => OrderState::Failed,
        PaymentExecutionOutcome::Authorized | PaymentExecutionOutcome::InterventionRequired => {
            OrderState::Authorized
        }
    };
    let receipt_state = match outcome {
        PaymentExecutionOutcome::Completed => ReceiptState::Settled,
        PaymentExecutionOutcome::Failed => ReceiptState::Failed,
        PaymentExecutionOutcome::Authorized => ReceiptState::Authorized,
        PaymentExecutionOutcome::InterventionRequired => ReceiptState::Pending,
    };

    SyncPaymentOutcomeCommand {
        context,
        outcome,
        order: Some(OrderSnapshot {
            order_id: record
                .and_then(|record| record.order.as_ref().and_then(|order| order.order_id.clone()))
                .or_else(|| Some(receipt.payment_mandate_id.clone())),
            receipt_id: Some(receipt.payment_id.clone()),
            state: order_state,
            receipt_state,
            extensions: ProtocolExtensions::default(),
        }),
        intervention: None,
        generated_evidence_refs: Vec::new(),
    }
}

pub(crate) fn update_record_extensions(
    record: &mut TransactionRecord,
    envelope: ProtocolExtensionEnvelope,
) {
    if !record.extensions.as_slice().contains(&envelope) {
        record.attach_extension(envelope);
    }
}

pub(crate) fn update_record_state_from_receipt(
    record: &mut TransactionRecord,
    receipt: &PaymentReceipt,
) {
    if record.order.is_none() {
        record.order = Some(OrderSnapshot {
            order_id: Some(receipt.payment_mandate_id.clone()),
            receipt_id: Some(receipt.payment_id.clone()),
            state: OrderState::Draft,
            receipt_state: ReceiptState::Pending,
            extensions: ProtocolExtensions::default(),
        });
    }

    if let Some(order) = &mut record.order {
        order.receipt_id = Some(receipt.payment_id.clone());
        match receipt.payment_status {
            PaymentStatusEnvelope::Success(_) => {
                order.state = OrderState::Completed;
                order.receipt_state = ReceiptState::Settled;
            }
            PaymentStatusEnvelope::Error(_) | PaymentStatusEnvelope::Failure(_) => {
                order.state = OrderState::Failed;
                order.receipt_state = ReceiptState::Failed;
            }
        }
    }

    match receipt.payment_status {
        PaymentStatusEnvelope::Success(_) => {
            record.state = TransactionState::Completed;
        }
        PaymentStatusEnvelope::Error(_) | PaymentStatusEnvelope::Failure(_) => {
            record.state = TransactionState::Failed;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::{Number, json};

    use super::*;

    /// One generated decimal amount: magnitude, fraction digits, and sign.
    fn decimal() -> impl Strategy<Value = (i64, u32)> {
        (0_i64..1_000_000_000, 0_u32..=4, any::<bool>()).prop_map(|(magnitude, scale, negative)| {
            (if negative { -magnitude } else { magnitude }, scale)
        })
    }

    fn number(amount_minor: i64, scale: u32) -> Number {
        Money::new("USD", amount_minor, scale).to_decimal_string().parse().unwrap()
    }

    fn mandate(currency: &str, items: &[(i64, u32)], total: (i64, u32)) -> CartMandate {
        let display_items: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(index, (amount_minor, scale))| {
                json!({
                    "label": format!("item-{index}"),
                    "amount": {"currency": currency, "value": number(*amount_minor, *scale)}
                })
            })
            .collect();
        serde_json::from_value(json!({
            "contents": {
                "id": "cart-prop",
                "user_cart_confirmation_required": false,
                "payment_request": {
                    "method_data": [{"supported_methods": "CARD"}],
                    "details": {
                        "id": "cart-prop",
                        "display_items": display_items,
                        "total": {
                            "label": "Total",
                            "amount": {"currency": currency, "value": number(total.0, total.1)}
                        }
                    }
                },
                "cart_expiry": "2099-01-01T00:00:00Z",
                "merchant_name": "Merchant"
            },
            "merchant_authorization": "signed"
        }))
        .unwrap()
    }

    /// Exact sum of `items` at scale `scale`, computed independently in `i128`.
    fn reference_sum(items: &[(i64, u32)], scale: u32) -> i128 {
        items
            .iter()
            .map(|(amount_minor, item_scale)| {
                i128::from(*amount_minor) * 10_i128.pow(scale - item_scale)
            })
            .sum()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn balanced_cart_has_one_scale_and_no_reconciliation(
            currency in prop::sample::select(vec!["USD", "JPY", "KWD", "XXX"]),
            items in prop::collection::vec(decimal(), 1..6),
        ) {
            let item_scale = items.iter().map(|(_, scale)| *scale).max().unwrap_or(0);
            let total_minor = i64::try_from(reference_sum(&items, item_scale)).unwrap();
            let cart = cart_from_cart_mandate(&mandate(currency, &items, (total_minor, item_scale)))
                .unwrap();

            let scale = cart.total.scale;
            let significant = items
                .iter()
                .chain(std::iter::once(&(total_minor, item_scale)))
                .map(|(amount, scale)| Money::new(currency, *amount, *scale))
                .map(|money| Money::parse_decimal(currency, &money.to_decimal_string()).unwrap().scale)
                .max()
                .unwrap_or(0);
            prop_assert_eq!(
                scale,
                Money::iso_minor_unit_scale(currency).unwrap_or(0).max(significant)
            );
            prop_assert!(cart.lines.iter().all(|line| line.total_price.scale == scale));
            let line_sum: i128 =
                cart.lines.iter().map(|line| i128::from(line.total_price.amount_minor)).sum();
            prop_assert_eq!(line_sum, i128::from(cart.total.amount_minor));
            prop_assert_eq!(cart.subtotal, Some(cart.total.clone()));
            prop_assert!(cart.adjustments.is_empty());
            prop_assert_eq!(
                cart.total.compare_amount(&Money::new(currency, total_minor, item_scale)),
                Some(std::cmp::Ordering::Equal)
            );
        }

        #[test]
        fn unbalanced_cart_reconciles_the_exact_difference(
            items in prop::collection::vec(decimal(), 1..6),
            total in decimal(),
        ) {
            let cart = cart_from_cart_mandate(&mandate("USD", &items, total)).unwrap();
            let scale = cart.total.scale;
            let lines: i128 =
                cart.lines.iter().map(|line| i128::from(line.total_price.amount_minor)).sum();
            let adjustments: i128 =
                cart.adjustments.iter().map(|adjustment| i128::from(adjustment.amount.amount_minor)).sum();

            prop_assert!(cart.adjustments.iter().all(|adjustment| adjustment.amount.scale == scale));
            prop_assert_eq!(lines + adjustments, i128::from(cart.total.amount_minor));
            prop_assert_eq!(cart.adjustments.is_empty(), lines == i128::from(cart.total.amount_minor));
        }
    }
}
