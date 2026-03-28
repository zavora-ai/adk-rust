use serde::{Deserialize, Serialize};

/// Currency-denominated amount stored as minor units plus an explicit scale.
///
/// `amount_minor` keeps commerce arithmetic exact and avoids floating-point
/// rounding drift.
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
}
