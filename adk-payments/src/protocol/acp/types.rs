use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AcpProtocolVersion {
    pub(crate) version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpBuyer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) phone_number: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpAddress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) line_one: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) line_two: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) postal_code: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpFulfillmentDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) phone_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) address: Option<AcpAddress>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpAffiliateAttribution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) publisher_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) touchpoint: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpInterventionCapabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) supported: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) required: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enforcement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) redirect_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_redirects: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_interaction_depth: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpPaymentHandler {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) spec: String,
    pub(crate) requires_delegate_payment: bool,
    pub(crate) requires_pci_compliance: bool,
    pub(crate) psp: String,
    pub(crate) config_schema: String,
    pub(crate) instrument_schemas: Vec<String>,
    pub(crate) config: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpPaymentCapabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) handlers: Vec<AcpPaymentHandler>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpCapabilities {
    #[serde(default)]
    pub(crate) payment: AcpPaymentCapabilities,
    #[serde(default)]
    pub(crate) interventions: AcpInterventionCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) extensions: Vec<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpRequestLineItem {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quantity: Option<u32>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpItemReference {
    pub(crate) id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpDisplayAttribute {
    pub(crate) display_name: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpDisclosure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpMarketplaceSellerDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpTotal {
    pub(crate) r#type: String,
    pub(crate) display_text: String,
    pub(crate) amount: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpLineItem {
    pub(crate) id: String,
    pub(crate) item: AcpItemReference,
    pub(crate) quantity: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) images: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) unit_amount: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) custom_attributes: Vec<AcpDisplayAttribute>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) disclosures: Vec<AcpDisclosure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) marketplace_seller_details: Option<AcpMarketplaceSellerDetails>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) totals: Vec<AcpTotal>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpFulfillmentOption {
    pub(crate) r#type: String,
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) carrier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) earliest_delivery_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) latest_delivery_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) totals: Vec<AcpTotal>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpSelectedFulfillmentOption {
    pub(crate) r#type: String,
    pub(crate) option_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) item_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpMessage {
    pub(crate) r#type: String,
    pub(crate) content_type: String,
    pub(crate) content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpLink {
    pub(crate) r#type: String,
    pub(crate) url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpOrder {
    pub(crate) id: String,
    pub(crate) checkout_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) permalink_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpCheckoutSession {
    pub(crate) id: String,
    pub(crate) protocol: AcpProtocolVersion,
    pub(crate) capabilities: AcpCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) buyer: Option<AcpBuyer>,
    pub(crate) status: String,
    pub(crate) currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) presentment_currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exchange_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exchange_rate_timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timezone: Option<String>,
    pub(crate) line_items: Vec<AcpLineItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fulfillment_details: Option<AcpFulfillmentDetails>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fulfillment_options: Vec<AcpFulfillmentOption>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) selected_fulfillment_options: Vec<AcpSelectedFulfillmentOption>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fulfillment_groups: Vec<Value>,
    pub(crate) totals: Vec<AcpTotal>,
    pub(crate) messages: Vec<AcpMessage>,
    pub(crate) links: Vec<AcpLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) authentication_metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continue_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) metadata: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quote_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) discounts: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) order: Option<AcpOrder>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpCreateCheckoutSessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) buyer: Option<AcpBuyer>,
    pub(crate) line_items: Vec<AcpRequestLineItem>,
    pub(crate) currency: String,
    #[serde(default)]
    pub(crate) capabilities: AcpCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fulfillment_details: Option<AcpFulfillmentDetails>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fulfillment_groups: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) affiliate_attribution: Option<AcpAffiliateAttribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) coupons: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) discounts: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) locale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quote_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpCheckoutSessionUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) buyer: Option<AcpBuyer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) line_items: Vec<AcpRequestLineItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fulfillment_details: Option<AcpFulfillmentDetails>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fulfillment_groups: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) selected_fulfillment_options: Vec<AcpSelectedFulfillmentOption>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) coupons: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) discounts: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpPaymentInstrumentCredential {
    pub(crate) r#type: String,
    pub(crate) token: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpPaymentInstrument {
    pub(crate) r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential: Option<AcpPaymentInstrumentCredential>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpPaymentData {
    pub(crate) handler_id: String,
    pub(crate) instrument: AcpPaymentInstrument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) billing_address: Option<AcpAddress>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpRiskSignal {
    pub(crate) r#type: String,
    pub(crate) score: i64,
    pub(crate) action: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpCheckoutSessionCompleteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) buyer: Option<AcpBuyer>,
    pub(crate) payment_data: AcpPaymentData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) authentication_result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) affiliate_attribution: Option<AcpAffiliateAttribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) risk_signals: Vec<AcpRiskSignal>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpIntentTrace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trace_summary: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpCancelSessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) intent_trace: Option<AcpIntentTrace>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpDelegatePaymentRequest {
    pub(crate) payment_method: AcpPaymentMethodCard,
    pub(crate) allowance: AcpAllowance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) billing_address: Option<AcpAddress>,
    pub(crate) risk_signals: Vec<AcpRiskSignal>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpPaymentMethodCard {
    pub(crate) r#type: String,
    pub(crate) card_number_type: String,
    pub(crate) number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exp_month: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exp_year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cvc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cryptogram: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) eci_value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) checks_performed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) iin: Option<String>,
    pub(crate) display_card_funding_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display_wallet_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display_brand: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display_last4: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) r#virtual: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpAllowance {
    pub(crate) reason: String,
    pub(crate) max_amount: i64,
    pub(crate) currency: String,
    pub(crate) checkout_session_id: String,
    pub(crate) merchant_id: String,
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpDelegatePaymentResponse {
    pub(crate) id: String,
    pub(crate) created: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AcpErrorResponse {
    pub(crate) r#type: String,
    pub(crate) code: String,
    pub(crate) message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) param: Option<String>,
}
