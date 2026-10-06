use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::domain::Money;
use crate::protocol::ap2::error::Ap2Error;

/// AP2 alpha A2A extension URI.
pub const AP2_A2A_EXTENSION_URI: &str = "https://github.com/google-agentic-commerce/ap2/tree/v0.1";

/// AP2 A2A message data key for `IntentMandate`.
pub const AP2_INTENT_MANDATE_DATA_KEY: &str = "ap2.mandates.IntentMandate";

/// AP2 A2A artifact data key for `CartMandate`.
pub const AP2_CART_MANDATE_DATA_KEY: &str = "ap2.mandates.CartMandate";

/// AP2 A2A message data key for `PaymentMandate`.
pub const AP2_PAYMENT_MANDATE_DATA_KEY: &str = "ap2.mandates.PaymentMandate";

/// AP2 data key for `PaymentReceipt`.
pub const AP2_PAYMENT_RECEIPT_DATA_KEY: &str = "ap2.PaymentReceipt";

/// AP2 data key for W3C `PaymentMethodData`.
pub const AP2_PAYMENT_METHOD_DATA_KEY: &str = "payment_request.PaymentMethodData";

/// AP2 data key for W3C `ContactAddress`.
pub const AP2_CONTACT_ADDRESS_DATA_KEY: &str = "contact_picker.ContactAddress";

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_refund_period() -> u32 {
    30
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_default_refund_period(value: &u32) -> bool {
    *value == default_refund_period()
}

fn utc_now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// AP2 roles advertised through mandate and A2A metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ap2Role {
    #[serde(rename = "shopper")]
    Shopper,
    #[serde(rename = "merchant")]
    Merchant,
    #[serde(rename = "credentials-provider")]
    CredentialsProvider,
    #[serde(rename = "payment-processor")]
    PaymentProcessor,
}

/// AP2 role metadata used in AgentCard extension params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2RoleMetadata {
    pub roles: Vec<Ap2Role>,
}

/// Detached authorization artifact supplied alongside AP2 mandates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationArtifact {
    pub artifact_type: String,
    pub value: String,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

impl AuthorizationArtifact {
    /// Creates one inline authorization artifact.
    #[must_use]
    pub fn new(
        artifact_type: impl Into<String>,
        value: impl Into<String>,
        content_type: impl Into<String>,
    ) -> Self {
        Self {
            artifact_type: artifact_type.into(),
            value: value.into(),
            content_type: content_type.into(),
            metadata: Map::new(),
        }
    }
}

/// AP2 representation of a contact address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContactAddress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependent_locality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sorting_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_line: Option<Vec<String>>,
}

/// W3C `PaymentCurrencyAmount` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentCurrencyAmount {
    pub currency: String,
    pub value: Number,
}

impl PaymentCurrencyAmount {
    /// Creates one payment amount from a floating-point major-unit value.
    #[must_use]
    pub fn new(currency: impl Into<String>, value: f64) -> Self {
        Self {
            currency: currency.into(),
            value: Number::from_f64(value).unwrap_or_else(|| Number::from(0)),
        }
    }

    /// Converts the AP2 amount into canonical money without rounding.
    ///
    /// The sign comes from the decimal string itself, so `-0.50 USD` becomes
    /// `-50` minor units. The scale is the currency's ISO 4217 minor-unit scale,
    /// raised to the number of significant fraction digits when the value is
    /// more precise than the currency's minor unit.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::domain::Money;
    /// use adk_payments::protocol::ap2::PaymentCurrencyAmount;
    ///
    /// let amount = PaymentCurrencyAmount::new("USD", 12.34);
    /// assert_eq!(amount.to_money().unwrap(), Money::new("USD", 1_234, 2));
    ///
    /// let yen = PaymentCurrencyAmount::new("JPY", 1_200.0);
    /// assert_eq!(yen.to_money().unwrap(), Money::new("JPY", 1_200, 0));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Ap2Error::InvalidAmount`] when the value is malformed, carries
    /// more than [`MAX_MONEY_SCALE`](crate::domain::MAX_MONEY_SCALE) significant
    /// fraction digits, or does not fit in `i64` minor units.
    pub fn to_money(&self) -> Result<Money, Ap2Error> {
        let exact = self.to_exact_money()?;
        let scale = Money::iso_minor_unit_scale(&self.currency).unwrap_or(0).max(exact.scale);
        Ok(exact.rescaled(scale)?)
    }

    /// Parses the value at its own significant scale, without any currency floor.
    pub(crate) fn to_exact_money(&self) -> Result<Money, Ap2Error> {
        Ok(Money::parse_decimal(self.currency.clone(), &self.value.to_string())?)
    }
}

/// W3C `PaymentItem` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentItem {
    pub label: String,
    pub amount: PaymentCurrencyAmount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<bool>,
    #[serde(default = "default_refund_period", skip_serializing_if = "is_default_refund_period")]
    pub refund_period: u32,
}

/// W3C `PaymentShippingOption` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentShippingOption {
    pub id: String,
    pub label: String,
    pub amount: PaymentCurrencyAmount,
    #[serde(default = "default_false", skip_serializing_if = "is_false")]
    pub selected: bool,
}

/// W3C `PaymentOptions` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentOptions {
    #[serde(default = "default_false", skip_serializing_if = "is_false")]
    pub request_payer_name: bool,
    #[serde(default = "default_false", skip_serializing_if = "is_false")]
    pub request_payer_email: bool,
    #[serde(default = "default_false", skip_serializing_if = "is_false")]
    pub request_payer_phone: bool,
    #[serde(default = "default_true", skip_serializing_if = "std::ops::Not::not")]
    pub request_shipping: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping_type: Option<String>,
}

impl Default for PaymentOptions {
    fn default() -> Self {
        Self {
            request_payer_name: false,
            request_payer_email: false,
            request_payer_phone: false,
            request_shipping: true,
            shipping_type: None,
        }
    }
}

/// W3C `PaymentMethodData` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentMethodData {
    pub supported_methods: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Map<String, Value>>,
}

/// W3C `PaymentDetailsModifier` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentDetailsModifier {
    pub supported_methods: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<PaymentItem>,
    #[serde(default, alias = "additionalDisplayItems", skip_serializing_if = "Option::is_none")]
    pub additional_display_items: Option<Vec<PaymentItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Map<String, Value>>,
}

/// W3C `PaymentDetailsInit` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentDetailsInit {
    pub id: String,
    #[serde(default, alias = "displayItems")]
    pub display_items: Vec<PaymentItem>,
    #[serde(default, alias = "shippingOptions", skip_serializing_if = "Option::is_none")]
    pub shipping_options: Option<Vec<PaymentShippingOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<Vec<PaymentDetailsModifier>>,
    pub total: PaymentItem,
}

/// W3C `PaymentRequest` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentRequest {
    pub method_data: Vec<PaymentMethodData>,
    pub details: PaymentDetailsInit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<PaymentOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping_address: Option<ContactAddress>,
}

/// W3C `PaymentResponse` projected through AP2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentResponse {
    pub request_id: String,
    pub method_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping_address: Option<ContactAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping_option: Option<PaymentShippingOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_phone: Option<String>,
}

/// AP2 alpha `IntentMandate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IntentMandate {
    #[serde(default = "default_true")]
    pub user_cart_confirmation_required: bool,
    pub natural_language_description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchants: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skus: Option<Vec<String>>,
    #[serde(
        default = "default_false",
        alias = "required_refundability",
        skip_serializing_if = "is_false"
    )]
    pub requires_refundability: bool,
    pub intent_expiry: String,
}

/// AP2 alpha cart contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CartContents {
    pub id: String,
    #[serde(alias = "user_signature_required")]
    pub user_cart_confirmation_required: bool,
    pub payment_request: PaymentRequest,
    pub cart_expiry: String,
    pub merchant_name: String,
}

/// AP2 alpha `CartMandate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CartMandate {
    pub contents: CartContents,
    #[serde(alias = "merchant_signature", default, skip_serializing_if = "Option::is_none")]
    pub merchant_authorization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// AP2 alpha payment mandate contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentMandateContents {
    pub payment_mandate_id: String,
    pub payment_details_id: String,
    pub payment_details_total: PaymentItem,
    pub payment_response: PaymentResponse,
    pub merchant_agent: String,
    #[serde(default = "utc_now_rfc3339")]
    pub timestamp: String,
}

/// AP2 alpha `PaymentMandate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentMandate {
    pub payment_mandate_contents: PaymentMandateContents,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_authorization: Option<String>,
}

/// Successful payment receipt status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentSuccessStatus {
    pub merchant_confirmation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psp_confirmation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_confirmation_id: Option<String>,
}

/// Errored payment receipt status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentErrorStatus {
    pub error_message: String,
}

/// Failed payment receipt status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentFailureStatus {
    pub failure_message: String,
}

/// Final AP2 payment receipt status union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaymentStatusEnvelope {
    Success(PaymentSuccessStatus),
    Error(PaymentErrorStatus),
    Failure(PaymentFailureStatus),
}

/// AP2 alpha `PaymentReceipt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaymentReceipt {
    pub payment_mandate_id: String,
    #[serde(default = "utc_now_rfc3339")]
    pub timestamp: String,
    pub payment_id: String,
    pub amount: PaymentCurrencyAmount,
    pub payment_status: PaymentStatusEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_method_details: Option<Map<String, Value>>,
}

impl PaymentReceipt {
    /// Returns the parsed receipt timestamp when the payload is well-formed.
    #[must_use]
    pub fn parsed_timestamp(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.timestamp).ok().map(|value| value.with_timezone(&Utc))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn amount(currency: &str, value: Value) -> PaymentCurrencyAmount {
        serde_json::from_value(json!({"currency": currency, "value": value})).unwrap()
    }

    #[test]
    fn amount_projection_preserves_minor_units() {
        let amount = PaymentCurrencyAmount::new("USD", 12.34);
        assert_eq!(amount.to_money().unwrap(), Money::new("USD", 1_234, 2));
    }

    #[test]
    fn negative_amount_below_one_major_unit_stays_negative() {
        assert_eq!(amount("USD", json!(-0.5)).to_money().unwrap(), Money::new("USD", -50, 2));
        assert_eq!(amount("USD", json!(-0.05)).to_money().unwrap(), Money::new("USD", -5, 2));
    }

    #[test]
    fn amount_scale_follows_currency_minor_unit() {
        assert_eq!(amount("JPY", json!(1200)).to_money().unwrap(), Money::new("JPY", 1_200, 0));
        assert_eq!(amount("KWD", json!(1.5)).to_money().unwrap(), Money::new("KWD", 1_500, 3));
        assert_eq!(amount("USD", json!(1.125)).to_money().unwrap(), Money::new("USD", 1_125, 3));
        assert_eq!(amount("XXX", json!(7)).to_money().unwrap(), Money::new("XXX", 7, 0));
    }

    #[test]
    fn amount_with_more_than_eighteen_fraction_digits_is_rejected() {
        let error = amount("USD", json!(1e-19)).to_money().unwrap_err();
        assert_eq!(
            error,
            Ap2Error::InvalidAmount(crate::domain::MoneyError::PrecisionTooHigh {
                value: "1e-19".to_string(),
                scale: 19,
                max_scale: crate::domain::MAX_MONEY_SCALE,
            })
        );
    }

    #[test]
    fn amount_overflow_is_an_error_not_a_panic() {
        let error = amount("USD", json!(1e300)).to_money().unwrap_err();
        assert!(matches!(
            error,
            Ap2Error::InvalidAmount(crate::domain::MoneyError::Overflow { .. })
        ));
        let error = amount("USD", json!(92_233_720_368_547_759_i64)).to_money().unwrap_err();
        assert!(matches!(
            error,
            Ap2Error::InvalidAmount(crate::domain::MoneyError::Overflow { .. })
        ));
    }

    #[test]
    fn cart_mandate_supports_documentation_aliases() {
        let mandate: CartMandate = serde_json::from_value(json!({
            "contents": {
                "id": "cart-1",
                "user_signature_required": false,
                "payment_request": {
                    "method_data": [{"supported_methods": "CARD"}],
                    "details": {
                        "id": "order-1",
                        "displayItems": [{
                            "label": "Shoes",
                            "amount": {"currency": "USD", "value": 120.0}
                        }],
                        "total": {
                            "label": "Total",
                            "amount": {"currency": "USD", "value": 120.0}
                        }
                    }
                },
                "cart_expiry": "2026-03-22T12:00:00Z",
                "merchant_name": "Merchant"
            },
            "merchant_signature": "signed"
        }))
        .unwrap();

        assert!(!mandate.contents.user_cart_confirmation_required);
        assert_eq!(mandate.merchant_authorization.as_deref(), Some("signed"));
    }
}
