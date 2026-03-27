use serde::{Deserialize, Serialize};

use crate::domain::{Money, ProtocolExtensions};

/// One authoritative line item in the canonical cart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CartLine {
    pub line_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant_sku: Option<String>,
    pub title: String,
    pub quantity: u32,
    pub unit_price: Money,
    pub total_price: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_class: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Type of canonical adjustment applied to a cart total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceAdjustmentKind {
    Tax,
    Shipping,
    Discount,
    Fee,
    Surcharge,
    Credit,
    Other(String),
}

/// Total component outside the primary cart lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceAdjustment {
    pub adjustment_id: String,
    pub kind: PriceAdjustmentKind,
    pub label: String,
    pub amount: Money,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Affiliate or attribution metadata preserved in the canonical cart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffiliateAttribution {
    pub partner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical cart contents and totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cart_id: Option<String>,
    pub lines: Vec<CartLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtotal: Option<Money>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adjustments: Vec<PriceAdjustment>,
    pub total: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affiliate_attribution: Option<AffiliateAttribution>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Canonical fulfillment mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FulfillmentKind {
    Shipping,
    Pickup,
    Delivery,
    Digital,
    Service,
    Other(String),
}

/// Delivery or pickup destination details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FulfillmentDestination {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
}

/// Fulfillment choice currently attached to the transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FulfillmentSelection {
    pub fulfillment_id: String,
    pub kind: FulfillmentKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<Money>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<FulfillmentDestination>,
    #[serde(default)]
    pub requires_user_selection: bool,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}
