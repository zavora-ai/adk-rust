//! Commerce: signed receipts and quotes (spec 3.5 §7.5, A-I25, A-I26).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::identity::{Proof, RequesterId};

/// One priced line of a quote or receipt (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct LineItem {
    /// Product or service SKU.
    pub sku: String,
    /// Line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Quantity.
    pub quantity: u32,
    /// Unit price in the currency's minor units, from a signed release (A-I26).
    pub unit_amount: u64,
}

impl LineItem {
    /// Returns `quantity × unitAmount`, or `None` on overflow.
    pub fn amount(&self) -> Option<u64> {
        self.unit_amount.checked_mul(u64::from(self.quantity))
    }
}

/// Sums line amounts, or `None` on overflow.
fn lines_total(lines: &[LineItem]) -> Option<u64> {
    lines.iter().try_fold(0u64, |total, line| total.checked_add(line.amount()?))
}

/// A quote that keeps its price until `validUntil` (spec 3.5 §7.5, A-I25).
///
/// # Example
///
/// ```
/// use awp_types::{LineItem, QuoteDocument};
/// use chrono::{Duration, Utc};
/// use uuid::Uuid;
///
/// let now = Utc::now();
/// let quote = QuoteDocument {
///     id: Uuid::nil(),
///     capability: "get_quote".into(),
///     requester: None,
///     lines: vec![LineItem { sku: "PANEL-400".into(), description: None, quantity: 2, unit_amount: 25_000 }],
///     total: 50_000,
///     currency: "KES".into(),
///     release_seq: 42,
///     issued_at: now,
///     valid_until: now + Duration::days(7),
///     proof: None,
/// };
/// assert!(quote.is_open(now));
/// assert!(!quote.is_open(now + Duration::days(8)));
/// assert_eq!(quote.lines_total(), Some(50_000));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct QuoteDocument {
    /// Quote id.
    pub id: Uuid,
    /// Name of the capability that issued it.
    pub capability: String,
    /// The verified requester it was issued to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<RequesterId>,
    /// Priced lines.
    pub lines: Vec<LineItem>,
    /// Total in minor units.
    pub total: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// The signed release whose prices the quote uses (A-I26).
    pub release_seq: u64,
    /// When the quote was issued.
    pub issued_at: DateTime<Utc>,
    /// The quote's price holds until this time (A-I25).
    pub valid_until: DateTime<Utc>,
    /// Detached JWS by a delegated `receipt` or `payments` key over the JCS form of the quote
    /// without `proof`; optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Proof>,
}

impl QuoteDocument {
    /// Returns `true` while the quote's price holds: `at` is not after `validUntil`.
    pub fn is_open(&self, at: DateTime<Utc>) -> bool {
        at <= self.valid_until
    }

    /// Returns the sum of the line amounts, or `None` on overflow.
    pub fn lines_total(&self) -> Option<u64> {
        lines_total(&self.lines)
    }
}

/// A commerce receipt signed by a delegated receipt key so any agent verifies it
/// (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    /// Receipt id.
    pub id: Uuid,
    /// Name of the capability the receipt is for: an order, booking or payment.
    pub capability: String,
    /// Order, booking or payment id.
    pub reference: String,
    /// The verified requester who paid or booked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<RequesterId>,
    /// The quote the receipt settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<Uuid>,
    /// Priced lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<LineItem>,
    /// Amount in minor units.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// The payment rail that settled it, such as `ap2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail: Option<String>,
    /// The signed release whose prices the receipt uses (A-I26).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_seq: Option<u64>,
    /// When the receipt was issued.
    pub issued_at: DateTime<Utc>,
    /// Detached JWS by a delegated `receipt` key over the JCS form of the receipt without
    /// `proof`.
    pub proof: Proof,
}

impl Receipt {
    /// Returns the sum of the line amounts, or `None` on overflow.
    pub fn lines_total(&self) -> Option<u64> {
        lines_total(&self.lines)
    }
}
