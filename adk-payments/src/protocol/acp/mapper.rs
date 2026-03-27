use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::ACP_STABLE_BASELINE;
use crate::domain::{
    AffiliateAttribution, Cart, CartLine, FulfillmentDestination, FulfillmentKind,
    FulfillmentSelection, InterventionKind, Money, PaymentMethodSelection, PriceAdjustmentKind,
    ProtocolDescriptor, ProtocolExtensionEnvelope, ProtocolExtensions, TransactionRecord,
    TransactionState,
};
use crate::kernel::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    DelegatePaymentAllowance, DelegatePaymentCommand, DelegatedRiskSignal, UpdateCheckoutCommand,
};
use crate::protocol::acp::types::{
    AcpAffiliateAttribution, AcpCancelSessionRequest, AcpCapabilities, AcpCheckoutSession,
    AcpCheckoutSessionCompleteRequest, AcpCheckoutSessionUpdateRequest,
    AcpCreateCheckoutSessionRequest, AcpDelegatePaymentRequest, AcpFulfillmentDetails,
    AcpFulfillmentOption, AcpInterventionCapabilities, AcpItemReference, AcpLineItem, AcpLink,
    AcpMarketplaceSellerDetails, AcpMessage, AcpOrder, AcpPaymentCapabilities, AcpPaymentData,
    AcpProtocolVersion, AcpRequestLineItem, AcpSelectedFulfillmentOption, AcpTotal,
};

struct CartRequestMetadata<'a> {
    currency: &'a str,
    affiliate_attribution: Option<&'a AcpAffiliateAttribution>,
    locale: Option<&'a str>,
    timezone: Option<&'a str>,
    quote_id: Option<&'a str>,
    metadata: &'a BTreeMap<String, Value>,
    discounts: Option<&'a Value>,
}

pub(crate) fn create_checkout_command(
    request: AcpCreateCheckoutSessionRequest,
    mut context: CommerceContext,
) -> CreateCheckoutCommand {
    context.extensions = merged_extensions(
        context.extensions,
        payload_extensions("create_checkout_session", json!(request.clone())),
    );

    CreateCheckoutCommand {
        context,
        cart: cart_from_request(
            &request.line_items,
            CartRequestMetadata {
                currency: &request.currency,
                affiliate_attribution: request.affiliate_attribution.as_ref(),
                locale: request.locale.as_deref(),
                timezone: request.timezone.as_deref(),
                quote_id: request.quote_id.as_deref(),
                metadata: &request.metadata,
                discounts: request.discounts.as_ref(),
            },
        ),
        fulfillment: fulfillment_from_request(
            request.fulfillment_details.as_ref(),
            &[],
            request.line_items.as_slice(),
        ),
    }
}

pub(crate) fn update_checkout_command(
    request: AcpCheckoutSessionUpdateRequest,
    mut context: CommerceContext,
) -> UpdateCheckoutCommand {
    context.extensions = merged_extensions(
        context.extensions,
        payload_extensions("update_checkout_session", json!(request.clone())),
    );

    UpdateCheckoutCommand {
        context,
        cart: (!request.line_items.is_empty()).then(|| {
            let metadata = BTreeMap::new();
            cart_from_request(
                &request.line_items,
                CartRequestMetadata {
                    currency: "usd",
                    affiliate_attribution: None,
                    locale: None,
                    timezone: None,
                    quote_id: None,
                    metadata: &metadata,
                    discounts: request.discounts.as_ref(),
                },
            )
        }),
        fulfillment: fulfillment_from_request(
            request.fulfillment_details.as_ref(),
            request.selected_fulfillment_options.as_slice(),
            request.line_items.as_slice(),
        ),
    }
}

pub(crate) fn complete_checkout_command(
    request: AcpCheckoutSessionCompleteRequest,
    context: CommerceContext,
) -> CompleteCheckoutCommand {
    CompleteCheckoutCommand {
        context,
        selected_payment_method: Some(payment_method_selection(&request.payment_data)),
        extensions: payload_extensions("complete_checkout_session", json!(request)),
    }
}

pub(crate) fn cancel_checkout_command(
    request: AcpCancelSessionRequest,
    context: CommerceContext,
) -> CancelCheckoutCommand {
    let reason = request
        .intent_trace
        .as_ref()
        .and_then(|trace| trace.reason_code.clone().or(trace.trace_summary.clone()));

    CancelCheckoutCommand {
        context,
        reason,
        extensions: payload_extensions("cancel_checkout_session", json!(request)),
    }
}

pub(crate) fn delegate_payment_command(
    request: AcpDelegatePaymentRequest,
    context: CommerceContext,
) -> DelegatePaymentCommand {
    let billing_address: Option<serde_json::Value> = if request.billing_address.is_some() {
        // Only record presence and country for risk/compliance — full address is PII
        let country = request.billing_address.as_ref().and_then(|a| a.country.clone());
        Some(json!({ "country": country }))
    } else {
        None
    };

    DelegatePaymentCommand {
        context,
        selected_payment_method: Some(PaymentMethodSelection {
            selection_kind: request.payment_method.r#type.clone(),
            reference: request.payment_method.display_last4.clone(),
            display_hint: request.payment_method.display_brand.clone(),
            extensions: payload_extensions(
                "delegate_payment_method",
                json!(request.payment_method.clone()),
            ),
        }),
        allowance: DelegatePaymentAllowance {
            reason: request.allowance.reason.clone(),
            max_amount: Money::new(
                request.allowance.currency.clone(),
                request.allowance.max_amount,
                2,
            ),
            merchant_id: request.allowance.merchant_id.clone(),
            checkout_session_id: request.allowance.checkout_session_id.clone(),
            expires_at: request.allowance.expires_at,
            extensions: payload_extensions("delegate_payment_allowance", json!(request.allowance)),
        },
        billing_address,
        risk_signals: request
            .risk_signals
            .iter()
            .map(|signal| DelegatedRiskSignal {
                signal_type: signal.r#type.clone(),
                score: signal.score,
                action: signal.action.clone(),
                extensions: payload_extensions("delegate_payment_risk_signal", json!(signal)),
            })
            .collect(),
        metadata: request.metadata.clone(),
        extensions: payload_extensions("delegate_payment", json!(request)),
    }
}

pub(crate) fn checkout_session_from_record(
    record: &TransactionRecord,
    include_order: bool,
) -> AcpCheckoutSession {
    let checkout_session_id = record
        .protocol_refs
        .acp_checkout_session_id
        .clone()
        .unwrap_or_else(|| record.transaction_id.as_str().to_string());
    let buyer = extension_field(record, "buyer");
    let capabilities =
        extension_field(record, "capabilities").unwrap_or_else(|| default_capabilities(record));
    let locale = extension_field(record, "locale");
    let timezone = extension_field(record, "timezone");
    let fulfillment_details = extension_field(record, "fulfillment_details")
        .or_else(|| fulfillment_details_from_record(record));
    let fulfillment_options = extension_field(record, "fulfillment_options")
        .unwrap_or_else(|| fulfillment_options_from_record(record));
    let selected_fulfillment_options = extension_field(record, "selected_fulfillment_options")
        .unwrap_or_else(|| selected_fulfillment_from_record(record));
    let line_items =
        extension_field(record, "line_items").unwrap_or_else(|| line_items_from_record(record));
    let totals = extension_field(record, "totals").unwrap_or_else(|| totals_from_record(record));
    let messages =
        extension_field(record, "messages").unwrap_or_else(|| messages_from_record(record));
    let links = extension_field(record, "links").unwrap_or_else(|| links_from_record(record));
    let metadata = extension_field(record, "metadata").unwrap_or_default();
    let discounts = extension_field(record, "discounts");
    let authentication_metadata = extension_field(record, "authentication_metadata");
    let quote_id = extension_field(record, "quote_id");
    let quote_expires_at = extension_field(record, "quote_expires_at");
    let presentment_currency = extension_field(record, "presentment_currency");
    let exchange_rate = extension_field(record, "exchange_rate");
    let exchange_rate_timestamp = extension_field(record, "exchange_rate_timestamp");
    let continue_url = extension_field(record, "continue_url");
    let expires_at = extension_field(record, "expires_at");

    AcpCheckoutSession {
        id: checkout_session_id.clone(),
        protocol: AcpProtocolVersion { version: ACP_STABLE_BASELINE.to_string() },
        capabilities,
        buyer,
        status: checkout_status(record),
        currency: record.cart.total.currency.to_lowercase(),
        presentment_currency,
        exchange_rate,
        exchange_rate_timestamp,
        locale,
        timezone,
        line_items,
        fulfillment_details,
        fulfillment_options,
        selected_fulfillment_options,
        fulfillment_groups: extension_field(record, "fulfillment_groups").unwrap_or_default(),
        totals,
        messages,
        links,
        authentication_metadata,
        created_at: Some(record.created_at),
        updated_at: Some(record.last_updated_at),
        expires_at,
        continue_url,
        metadata,
        quote_id,
        quote_expires_at,
        discounts,
        order: include_order.then(|| order_from_record(record, &checkout_session_id)),
    }
}

pub(crate) fn request_metadata_extensions(
    operation: &str,
    api_version: &str,
    request_id: Option<&str>,
    idempotency_key: Option<&str>,
    timestamp: Option<DateTime<Utc>>,
    signature_present: bool,
) -> ProtocolExtensions {
    let mut envelope = ProtocolExtensionEnvelope::new(protocol_descriptor())
        .with_field("operation", json!(operation))
        .with_field("api_version", json!(api_version))
        .with_field("signature_present", json!(signature_present));

    if let Some(request_id) = request_id {
        envelope = envelope.with_field("request_id", json!(request_id));
    }

    if let Some(idempotency_key) = idempotency_key {
        envelope = envelope.with_field("idempotency_key", json!(idempotency_key));
    }

    if let Some(timestamp) = timestamp {
        envelope = envelope.with_field("timestamp", json!(timestamp));
    }

    ProtocolExtensions::from(vec![envelope])
}

fn cart_from_request(
    line_items: &[AcpRequestLineItem],
    request_metadata: CartRequestMetadata<'_>,
) -> Cart {
    let normalized_currency = request_metadata.currency.to_lowercase();
    let lines: Vec<_> = line_items
        .iter()
        .map(|line_item| {
            let title = line_item
                .extra
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&line_item.id)
                .to_string();
            let quantity = line_item.quantity.unwrap_or(1);
            let amount = Money::new(normalized_currency.clone(), 0, 2);
            let line_extensions = payload_extensions("line_item", json!(line_item));

            CartLine {
                line_id: line_item.id.clone(),
                merchant_sku: Some(line_item.id.clone()),
                title,
                quantity,
                unit_price: amount.clone(),
                total_price: amount,
                product_class: line_item
                    .extra
                    .get("product_class")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                extensions: line_extensions,
            }
        })
        .collect();

    let mut cart_extensions = ProtocolExtensions::default();
    let mut cart_envelope = ProtocolExtensionEnvelope::new(protocol_descriptor());
    if let Some(locale) = request_metadata.locale {
        cart_envelope.fields.insert("locale".to_string(), json!(locale));
    }
    if let Some(timezone) = request_metadata.timezone {
        cart_envelope.fields.insert("timezone".to_string(), json!(timezone));
    }
    if let Some(quote_id) = request_metadata.quote_id {
        cart_envelope.fields.insert("quote_id".to_string(), json!(quote_id));
    }
    if !request_metadata.metadata.is_empty() {
        cart_envelope.fields.insert(
            "metadata".to_string(),
            Value::Object(request_metadata.metadata.clone().into_iter().collect::<Map<_, _>>()),
        );
    }
    if let Some(discounts) = request_metadata.discounts {
        cart_envelope.fields.insert("discounts".to_string(), discounts.clone());
    }
    if !cart_envelope.is_empty() {
        cart_extensions.push(cart_envelope);
    }

    Cart {
        cart_id: None,
        lines,
        subtotal: Some(Money::new(normalized_currency.clone(), 0, 2)),
        adjustments: Vec::new(),
        total: Money::new(normalized_currency, 0, 2),
        affiliate_attribution: request_metadata.affiliate_attribution.map(affiliate_from_request),
        extensions: cart_extensions,
    }
}

fn affiliate_from_request(affiliate: &AcpAffiliateAttribution) -> AffiliateAttribution {
    AffiliateAttribution {
        partner_id: affiliate
            .provider
            .clone()
            .unwrap_or_else(|| "unknown_affiliate_partner".to_string()),
        campaign_id: affiliate.publisher_id.clone(),
        source: affiliate.touchpoint.clone(),
        extensions: payload_extensions("affiliate_attribution", json!(affiliate)),
    }
}

fn fulfillment_from_request(
    fulfillment_details: Option<&AcpFulfillmentDetails>,
    selected_options: &[AcpSelectedFulfillmentOption],
    line_items: &[AcpRequestLineItem],
) -> Option<FulfillmentSelection> {
    let selected = selected_options.first();
    let fulfillment_type = selected.map(|option| option.r#type.as_str()).unwrap_or("shipping");
    let kind = match fulfillment_type {
        "pickup" => FulfillmentKind::Pickup,
        "local_delivery" => FulfillmentKind::Delivery,
        "digital" => FulfillmentKind::Digital,
        "service" => FulfillmentKind::Service,
        "shipping" => FulfillmentKind::Shipping,
        other => FulfillmentKind::Other(other.to_string()),
    };
    let option_id = selected
        .map(|option| option.option_id.clone())
        .or_else(|| fulfillment_details.as_ref().map(|_| "fulfillment_pending".to_string()))?;
    let item_ids: Vec<String> = if let Some(selected) = selected {
        selected.item_ids.clone()
    } else {
        line_items.iter().map(|line| line.id.clone()).collect()
    };
    let label = selected
        .map(|option| option.option_id.clone())
        .unwrap_or_else(|| "fulfillment pending".to_string());

    Some(FulfillmentSelection {
        fulfillment_id: option_id,
        kind,
        label,
        amount: None,
        destination: fulfillment_details.and_then(destination_from_fulfillment_details),
        requires_user_selection: selected.is_none(),
        extensions: payload_extensions(
            "selected_fulfillment_option",
            json!({
                "selected_fulfillment_options": selected_options,
                "item_ids": item_ids,
            }),
        ),
    })
}

fn destination_from_fulfillment_details(
    details: &AcpFulfillmentDetails,
) -> Option<FulfillmentDestination> {
    let address = details.address.as_ref()?;
    Some(FulfillmentDestination {
        recipient_name: details.name.clone().or_else(|| address.name.clone()),
        locality: address.city.clone(),
        region: address.state.clone(),
        country_code: address.country.clone(),
        postal_code: address.postal_code.clone(),
    })
}

fn payment_method_selection(payment_data: &AcpPaymentData) -> PaymentMethodSelection {
    let credential_reference =
        payment_data.instrument.credential.as_ref().map(|credential| credential.token.clone());

    PaymentMethodSelection {
        selection_kind: payment_data.handler_id.clone(),
        reference: credential_reference,
        display_hint: payment_data
            .instrument
            .credential
            .as_ref()
            .map(|credential| credential.r#type.clone()),
        extensions: payload_extensions("payment_data", json!(payment_data)),
    }
}

fn protocol_descriptor() -> ProtocolDescriptor {
    ProtocolDescriptor::acp(ACP_STABLE_BASELINE)
}

fn payload_extensions(operation: &str, payload: Value) -> ProtocolExtensions {
    let mut envelope = ProtocolExtensionEnvelope::new(protocol_descriptor())
        .with_field("operation", json!(operation))
        .with_field("request", payload.clone());

    if let Value::Object(map) = payload {
        for (key, value) in map {
            envelope.fields.insert(key, value);
        }
    }

    ProtocolExtensions::from(vec![envelope])
}

fn merged_extensions(
    mut left: ProtocolExtensions,
    right: ProtocolExtensions,
) -> ProtocolExtensions {
    left.0.extend(right.0);
    left
}

fn extension_field<T>(record: &TransactionRecord, key: &str) -> Option<T>
where
    T: DeserializeOwned,
{
    record
        .extensions
        .as_slice()
        .iter()
        .rev()
        .filter(|envelope| envelope.protocol.name == "acp")
        .find_map(|envelope| envelope.fields.get(key).cloned())
        .and_then(|value| serde_json::from_value(value).ok())
}

fn line_items_from_record(record: &TransactionRecord) -> Vec<AcpLineItem> {
    record
        .cart
        .lines
        .iter()
        .map(|line| AcpLineItem {
            id: line.line_id.clone(),
            item: AcpItemReference {
                id: line.merchant_sku.clone().unwrap_or_else(|| line.line_id.clone()),
            },
            quantity: line.quantity,
            name: Some(line.title.clone()),
            description: line_extension_field(line, "description"),
            images: line_extension_field(line, "images").unwrap_or_default(),
            unit_amount: Some(line.unit_price.amount_minor),
            custom_attributes: line_extension_field(line, "custom_attributes").unwrap_or_default(),
            disclosures: line_extension_field(line, "disclosures").unwrap_or_default(),
            marketplace_seller_details: line_extension_field(line, "marketplace_seller_details")
                .or_else(|| {
                    line_extension_field::<String>(line, "marketplace_seller_name")
                        .map(|name| AcpMarketplaceSellerDetails { name: Some(name) })
                }),
            totals: line_totals(line),
        })
        .collect()
}

fn line_totals(line: &CartLine) -> Vec<AcpTotal> {
    vec![
        AcpTotal {
            r#type: "items_base_amount".to_string(),
            display_text: "Base Amount".to_string(),
            amount: line.total_price.amount_minor,
            description: None,
        },
        AcpTotal {
            r#type: "subtotal".to_string(),
            display_text: "Subtotal".to_string(),
            amount: line.total_price.amount_minor,
            description: None,
        },
        AcpTotal {
            r#type: "total".to_string(),
            display_text: "Total".to_string(),
            amount: line.total_price.amount_minor,
            description: None,
        },
    ]
}

fn totals_from_record(record: &TransactionRecord) -> Vec<AcpTotal> {
    let items_total: i64 = record.cart.lines.iter().map(|line| line.total_price.amount_minor).sum();
    let mut totals = vec![
        AcpTotal {
            r#type: "items_base_amount".to_string(),
            display_text: "Item(s) total".to_string(),
            amount: items_total,
            description: None,
        },
        AcpTotal {
            r#type: "subtotal".to_string(),
            display_text: "Subtotal".to_string(),
            amount: record
                .cart
                .subtotal
                .as_ref()
                .map_or(items_total, |subtotal| subtotal.amount_minor),
            description: None,
        },
    ];

    totals.extend(record.cart.adjustments.iter().map(|adjustment| AcpTotal {
        r#type: adjustment_type(&adjustment.kind),
        display_text: adjustment.label.clone(),
        amount: adjustment.amount.amount_minor,
        description: None,
    }));
    totals.push(AcpTotal {
        r#type: "total".to_string(),
        display_text: "Total".to_string(),
        amount: record.cart.total.amount_minor,
        description: None,
    });
    totals
}

fn adjustment_type(kind: &PriceAdjustmentKind) -> String {
    match kind {
        PriceAdjustmentKind::Tax => "tax".to_string(),
        PriceAdjustmentKind::Shipping => "fulfillment".to_string(),
        PriceAdjustmentKind::Discount => "discount".to_string(),
        PriceAdjustmentKind::Fee => "fee".to_string(),
        PriceAdjustmentKind::Surcharge => "surcharge".to_string(),
        PriceAdjustmentKind::Credit => "credit".to_string(),
        PriceAdjustmentKind::Other(other) => other.clone(),
    }
}

fn fulfillment_options_from_record(record: &TransactionRecord) -> Vec<AcpFulfillmentOption> {
    record
        .fulfillment
        .as_ref()
        .map(|fulfillment| {
            vec![AcpFulfillmentOption {
                r#type: fulfillment_type(&fulfillment.kind),
                id: fulfillment.fulfillment_id.clone(),
                title: fulfillment.label.clone(),
                description: None,
                carrier: None,
                earliest_delivery_time: None,
                latest_delivery_time: None,
                totals: fulfillment.amount.clone().map_or_else(Vec::new, |amount| {
                    vec![AcpTotal {
                        r#type: "total".to_string(),
                        display_text: "Fulfillment".to_string(),
                        amount: amount.amount_minor,
                        description: None,
                    }]
                }),
            }]
        })
        .unwrap_or_default()
}

fn selected_fulfillment_from_record(
    record: &TransactionRecord,
) -> Vec<AcpSelectedFulfillmentOption> {
    record
        .fulfillment
        .as_ref()
        .map(|fulfillment| {
            vec![AcpSelectedFulfillmentOption {
                r#type: fulfillment_type(&fulfillment.kind),
                option_id: fulfillment.fulfillment_id.clone(),
                item_ids: record
                    .cart
                    .lines
                    .iter()
                    .map(|line| line.merchant_sku.clone().unwrap_or_else(|| line.line_id.clone()))
                    .collect(),
            }]
        })
        .unwrap_or_default()
}

fn fulfillment_details_from_record(record: &TransactionRecord) -> Option<AcpFulfillmentDetails> {
    let _ = record;
    None
}

fn default_capabilities(record: &TransactionRecord) -> AcpCapabilities {
    let handlers = extension_field(record, "payment_handlers").unwrap_or_default();
    let mut interventions: AcpInterventionCapabilities =
        extension_field(record, "interventions").unwrap_or_default();

    if let TransactionState::InterventionRequired(intervention) = &record.state {
        let required = match &intervention.kind {
            InterventionKind::ThreeDsChallenge => "3ds",
            InterventionKind::AddressVerification => "address_verification",
            InterventionKind::BiometricConfirmation => "biometric_confirmation",
            InterventionKind::BuyerReconfirmation => "buyer_reconfirmation",
            InterventionKind::MerchantReview => "merchant_review",
            InterventionKind::Other(other) => other,
        };
        if !interventions.supported.iter().any(|item| item == required) {
            interventions.supported.push(required.to_string());
        }
        if !interventions.required.iter().any(|item| item == required) {
            interventions.required.push(required.to_string());
        }
        if interventions.enforcement.is_none() {
            interventions.enforcement = Some("conditional".to_string());
        }
    }

    AcpCapabilities {
        payment: AcpPaymentCapabilities { handlers },
        interventions,
        extensions: extension_field(record, "capability_extensions").unwrap_or_default(),
    }
}

fn messages_from_record(record: &TransactionRecord) -> Vec<AcpMessage> {
    match &record.state {
        TransactionState::Canceled => vec![AcpMessage {
            r#type: "info".to_string(),
            content_type: "plain".to_string(),
            content: "Checkout session has been canceled.".to_string(),
        }],
        TransactionState::Failed => vec![AcpMessage {
            r#type: "error".to_string(),
            content_type: "plain".to_string(),
            content: "Checkout session requires merchant or buyer attention.".to_string(),
        }],
        TransactionState::InterventionRequired(intervention) => vec![AcpMessage {
            r#type: "warning".to_string(),
            content_type: "plain".to_string(),
            content: intervention.instructions.clone().unwrap_or_else(|| {
                "A payment intervention is required before completion.".to_string()
            }),
        }],
        _ => Vec::new(),
    }
}

fn links_from_record(record: &TransactionRecord) -> Vec<AcpLink> {
    record
        .merchant_of_record
        .website
        .as_ref()
        .map(|website| {
            vec![AcpLink {
                r#type: "merchant_home".to_string(),
                url: website.clone(),
                title: Some(record.merchant_of_record.legal_name.clone()),
            }]
        })
        .unwrap_or_default()
}

fn order_from_record(record: &TransactionRecord, checkout_session_id: &str) -> AcpOrder {
    let order_id = record
        .order
        .as_ref()
        .and_then(|order| order.order_id.clone())
        .or_else(|| record.protocol_refs.acp_order_id.clone())
        .unwrap_or_else(|| format!("ord_{}", record.transaction_id.as_str()));
    let permalink_url = extension_field(record, "order_permalink_url").or_else(|| {
        record
            .merchant_of_record
            .website
            .as_ref()
            .map(|website| format!("{website}/orders/{order_id}"))
    });

    AcpOrder { id: order_id, checkout_session_id: checkout_session_id.to_string(), permalink_url }
}

fn checkout_status(record: &TransactionRecord) -> String {
    match &record.state {
        TransactionState::Draft | TransactionState::Negotiating => "incomplete".to_string(),
        TransactionState::AwaitingUserAuthorization => "pending_approval".to_string(),
        TransactionState::AwaitingPaymentMethod => "ready_for_payment".to_string(),
        TransactionState::InterventionRequired(intervention) => match &intervention.kind {
            InterventionKind::ThreeDsChallenge => "authentication_required".to_string(),
            _ => "requires_escalation".to_string(),
        },
        TransactionState::Authorized => "complete_in_progress".to_string(),
        TransactionState::Completed => "completed".to_string(),
        TransactionState::Canceled => "canceled".to_string(),
        TransactionState::Failed => "requires_escalation".to_string(),
    }
}

fn fulfillment_type(kind: &FulfillmentKind) -> String {
    match kind {
        FulfillmentKind::Shipping => "shipping".to_string(),
        FulfillmentKind::Pickup => "pickup".to_string(),
        FulfillmentKind::Delivery => "local_delivery".to_string(),
        FulfillmentKind::Digital => "digital".to_string(),
        FulfillmentKind::Service => "service".to_string(),
        FulfillmentKind::Other(other) => other.clone(),
    }
}

fn line_extension_field<T>(line: &CartLine, key: &str) -> Option<T>
where
    T: DeserializeOwned,
{
    line.extensions
        .as_slice()
        .iter()
        .rev()
        .filter(|envelope| envelope.protocol.name == "acp")
        .find_map(|envelope| envelope.fields.get(key).cloned())
        .and_then(|value| serde_json::from_value(value).ok())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{
        CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, OrderSnapshot, OrderState,
        PriceAdjustment, ReceiptState, TransactionId,
    };
    use crate::protocol::acp::types::{AcpAllowance, AcpRiskSignal};

    fn context(transaction_id: &str) -> CommerceContext {
        CommerceContext {
            transaction_id: TransactionId::from(transaction_id),
            session_identity: None,
            actor: CommerceActor {
                actor_id: "shopper-agent".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("shopper".to_string()),
                tenant_id: Some("tenant-1".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            merchant_of_record: MerchantRef {
                merchant_id: "merchant-123".to_string(),
                legal_name: "Merchant Example LLC".to_string(),
                display_name: Some("Merchant Example".to_string()),
                statement_descriptor: Some("MERCHANT*EXAMPLE".to_string()),
                country_code: Some("US".to_string()),
                website: Some("https://merchant.example".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            payment_processor: None,
            mode: CommerceMode::HumanPresent,
            protocol: protocol_descriptor(),
            extensions: ProtocolExtensions::default(),
        }
    }

    #[test]
    fn maps_create_request_into_canonical_command_and_preserves_payload() {
        let command = create_checkout_command(
            AcpCreateCheckoutSessionRequest {
                line_items: vec![AcpRequestLineItem {
                    id: "item_123".to_string(),
                    quantity: Some(2),
                    extra: BTreeMap::from([("name".to_string(), json!("Vintage Jacket"))]),
                }],
                currency: "usd".to_string(),
                capabilities: AcpCapabilities::default(),
                locale: Some("en-US".to_string()),
                ..AcpCreateCheckoutSessionRequest::default()
            },
            context("checkout_session_123"),
        );

        assert_eq!(command.cart.lines.len(), 1);
        assert_eq!(command.cart.lines[0].title, "Vintage Jacket");
        assert_eq!(command.context.transaction_id.as_str(), "checkout_session_123");
        assert!(command.context.extensions.as_slice()[0].fields.contains_key("request"));
    }

    #[test]
    fn builds_complete_response_with_synthesized_order() {
        let mut record = TransactionRecord::new(
            TransactionId::from("checkout_session_123"),
            context("checkout_session_123").actor,
            context("checkout_session_123").merchant_of_record,
            CommerceMode::HumanPresent,
            Cart {
                cart_id: Some("cart-1".to_string()),
                lines: vec![CartLine {
                    line_id: "line_item_123".to_string(),
                    merchant_sku: Some("item_123".to_string()),
                    title: "Vintage Jacket".to_string(),
                    quantity: 1,
                    unit_price: Money::new("usd", 300, 2),
                    total_price: Money::new("usd", 300, 2),
                    product_class: None,
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("usd", 300, 2)),
                adjustments: vec![PriceAdjustment {
                    adjustment_id: "tax".to_string(),
                    kind: PriceAdjustmentKind::Tax,
                    label: "Tax".to_string(),
                    amount: Money::new("usd", 30, 2),
                    extensions: ProtocolExtensions::default(),
                }],
                total: Money::new("usd", 330, 2),
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
        );
        record
            .transition_to(
                TransactionState::Completed,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 5, 0).unwrap(),
            )
            .unwrap_err();
        record
            .transition_to(
                TransactionState::Negotiating,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 1, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::AwaitingPaymentMethod,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 2, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::Authorized,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 3, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::Completed,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 4, 0).unwrap(),
            )
            .unwrap();
        record.order = Some(OrderSnapshot {
            order_id: None,
            receipt_id: Some("receipt_123".to_string()),
            state: OrderState::Completed,
            receipt_state: ReceiptState::Settled,
            extensions: ProtocolExtensions::default(),
        });

        let session = checkout_session_from_record(&record, true);
        assert_eq!(session.status, "completed");
        assert_eq!(session.order.unwrap().checkout_session_id, "checkout_session_123");
    }

    #[test]
    fn maps_delegate_payment_request_into_canonical_delegate_command() {
        let command = delegate_payment_command(
            AcpDelegatePaymentRequest {
                payment_method: crate::protocol::acp::types::AcpPaymentMethodCard {
                    r#type: "card".to_string(),
                    card_number_type: "fpan".to_string(),
                    number: "4242424242424242".to_string(),
                    exp_month: Some("11".to_string()),
                    exp_year: Some("2026".to_string()),
                    name: Some("Jane Doe".to_string()),
                    cvc: Some("223".to_string()),
                    cryptogram: None,
                    eci_value: None,
                    checks_performed: vec!["avs".to_string()],
                    iin: Some("424242".to_string()),
                    display_card_funding_type: "credit".to_string(),
                    display_wallet_type: None,
                    display_brand: Some("visa".to_string()),
                    display_last4: Some("4242".to_string()),
                    metadata: BTreeMap::new(),
                    r#virtual: Some(false),
                },
                allowance: AcpAllowance {
                    reason: "one_time".to_string(),
                    max_amount: 2_000,
                    currency: "usd".to_string(),
                    checkout_session_id: "checkout_session_123".to_string(),
                    merchant_id: "merchant-123".to_string(),
                    expires_at: Utc.with_ymd_and_hms(2026, 3, 22, 11, 0, 0).unwrap(),
                },
                billing_address: None,
                risk_signals: vec![AcpRiskSignal {
                    r#type: "card_testing".to_string(),
                    score: 10,
                    action: "manual_review".to_string(),
                }],
                metadata: BTreeMap::from([("source".to_string(), "chatgpt_checkout".to_string())]),
            },
            context("checkout_session_123"),
        );

        assert_eq!(command.allowance.max_amount.amount_minor, 2_000);
        assert_eq!(
            command.selected_payment_method.as_ref().and_then(|method| method.reference.as_deref()),
            Some("4242")
        );
        assert!(command.extensions.as_slice()[0].fields.contains_key("payment_method"));
    }
}
