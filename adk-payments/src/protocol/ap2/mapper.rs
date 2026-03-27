use serde_json::Value;

use crate::AP2_ALPHA_BASELINE;
use crate::domain::{
    Cart, CartLine, FulfillmentKind, FulfillmentSelection, Money, OrderSnapshot, OrderState,
    PaymentMethodSelection, PriceAdjustment, PriceAdjustmentKind, ProtocolDescriptor,
    ProtocolExtensionEnvelope, ProtocolExtensions, ReceiptState, TransactionRecord,
    TransactionState,
};
use crate::kernel::{
    CommerceContext, CreateCheckoutCommand, ExecutePaymentCommand, PaymentExecutionOutcome,
    SyncPaymentOutcomeCommand, UpdateCheckoutCommand,
};
use crate::protocol::ap2::types::{
    CartMandate, IntentMandate, PaymentMandate, PaymentReceipt, PaymentStatusEnvelope,
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

pub(crate) fn cart_from_cart_mandate(mandate: &CartMandate) -> Cart {
    let display_items = &mandate.contents.payment_request.details.display_items;
    let mut lines = Vec::with_capacity(display_items.len());
    let mut subtotal_minor = 0_i64;

    for (index, item) in display_items.iter().enumerate() {
        let line_total = item.amount.to_money();
        subtotal_minor = subtotal_minor.saturating_add(line_total.amount_minor);
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

    let total = mandate.contents.payment_request.details.total.amount.to_money();
    let mut adjustments = Vec::new();
    if let Some(options) = &mandate.contents.payment_request.details.shipping_options {
        for option in options.iter().filter(|option| option.selected) {
            adjustments.push(PriceAdjustment {
                adjustment_id: option.id.clone(),
                kind: PriceAdjustmentKind::Shipping,
                label: option.label.clone(),
                amount: option.amount.to_money(),
                extensions: ProtocolExtensions::default(),
            });
        }
    }

    if let Some(modifiers) = &mandate.contents.payment_request.details.modifiers {
        for modifier in modifiers {
            if let Some(items) = &modifier.additional_display_items {
                for (index, item) in items.iter().enumerate() {
                    adjustments.push(PriceAdjustment {
                        adjustment_id: format!("{}:{index}", modifier.supported_methods),
                        kind: PriceAdjustmentKind::Fee,
                        label: item.label.clone(),
                        amount: item.amount.to_money(),
                        extensions: ProtocolExtensions::default(),
                    });
                }
            }
        }
    }

    let allocated_minor = subtotal_minor
        .saturating_add(adjustments.iter().map(|adjustment| adjustment.amount.amount_minor).sum());
    if allocated_minor != total.amount_minor {
        adjustments.push(PriceAdjustment {
            adjustment_id: "ap2_unallocated_delta".to_string(),
            kind: PriceAdjustmentKind::Other("ap2".to_string()),
            label: "AP2 total reconciliation".to_string(),
            amount: Money::new(
                total.currency.clone(),
                total.amount_minor.saturating_sub(allocated_minor),
                total.scale,
            ),
            extensions: ProtocolExtensions::default(),
        });
    }

    Cart {
        cart_id: Some(mandate.contents.id.clone()),
        lines,
        subtotal: Some(Money::new(total.currency.clone(), subtotal_minor, total.scale)),
        adjustments,
        total,
        affiliate_attribution: None,
        extensions: ProtocolExtensions::default(),
    }
}

pub(crate) fn fulfillment_from_cart_mandate(mandate: &CartMandate) -> Option<FulfillmentSelection> {
    mandate
        .contents
        .payment_request
        .details
        .shipping_options
        .as_ref()
        .and_then(|options| options.iter().find(|option| option.selected))
        .map(|option| FulfillmentSelection {
            fulfillment_id: option.id.clone(),
            kind: FulfillmentKind::Shipping,
            label: option.label.clone(),
            amount: Some(option.amount.to_money()),
            destination: None,
            requires_user_selection: mandate
                .contents
                .payment_request
                .options
                .as_ref()
                .is_some_and(|options| options.request_shipping),
            extensions: ProtocolExtensions::default(),
        })
}

pub(crate) fn intent_create_checkout_command(
    intent: &IntentMandate,
    context: CommerceContext,
) -> CreateCheckoutCommand {
    CreateCheckoutCommand { context, cart: placeholder_cart_from_intent(intent), fulfillment: None }
}

pub(crate) fn cart_create_checkout_command(
    mandate: &CartMandate,
    context: CommerceContext,
) -> CreateCheckoutCommand {
    CreateCheckoutCommand {
        context,
        cart: cart_from_cart_mandate(mandate),
        fulfillment: fulfillment_from_cart_mandate(mandate),
    }
}

pub(crate) fn cart_update_checkout_command(
    mandate: &CartMandate,
    context: CommerceContext,
) -> UpdateCheckoutCommand {
    UpdateCheckoutCommand {
        context,
        cart: Some(cart_from_cart_mandate(mandate)),
        fulfillment: fulfillment_from_cart_mandate(mandate),
    }
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

pub(crate) fn execute_payment_command(
    mandate: &PaymentMandate,
    context: CommerceContext,
    supporting_evidence_refs: Vec<crate::domain::EvidenceReference>,
) -> ExecutePaymentCommand {
    ExecutePaymentCommand {
        context,
        amount: mandate.payment_mandate_contents.payment_details_total.amount.to_money(),
        selected_payment_method: Some(payment_method_selection(mandate)),
        supporting_evidence_refs,
        extensions: ProtocolExtensions::default(),
    }
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
