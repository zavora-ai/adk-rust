//! Vendor-neutral usage units and per-unit prices.
//!
//! Token-billed model calls report [`UsageMetadata`](crate::UsageMetadata). Calls billed in
//! other units — generated images, seconds of audio transcribed, characters synthesized —
//! report a [`UsageVector`]: an amount per [`UsageUnit`]. A [`UnitRate`] prices one unit in
//! integer micro-USD, so cost stays the canonical rollup across every unit.
//!
//! | Unit | Counts | Typical vendor billing |
//! |------|--------|------------------------|
//! | [`InputTokens`](UsageUnit::InputTokens) | Tokens sent to a model | Per million tokens |
//! | [`OutputTokens`](UsageUnit::OutputTokens) | Tokens a model generated | Per million tokens |
//! | [`Images`](UsageUnit::Images) | Images generated, edited or analysed | Per image, per thousand images |
//! | [`AudioMillis`](UsageUnit::AudioMillis) | Milliseconds of audio processed or produced | Per hour, per minute |
//! | [`VideoMillis`](UsageUnit::VideoMillis) | Milliseconds of video processed or produced | Per second |
//! | [`Characters`](UsageUnit::Characters) | Characters of text synthesized or translated | Per thousand characters |
//! | [`Pages`](UsageUnit::Pages) | Document pages processed | Per page |
//! | [`Requests`](UsageUnit::Requests) | Requests billed as a whole | Per request |
//!
//! # Example
//!
//! ```
//! use adk_core::{UnitRate, UsageUnit, UsageVector};
//!
//! // $0.18 per audio hour, billed in whole seconds rounded down.
//! let rate = UnitRate::new(UsageUnit::AudioMillis, 180_000, 3_600_000).rounded_down_to(1_000);
//! let usage = UsageVector::new().with(UsageUnit::AudioMillis, 90_400);
//! assert_eq!(rate.cost_micro_usd(usage.get(UsageUnit::AudioMillis)), 4_500);
//! assert_eq!(serde_json::to_value(&usage).unwrap(), serde_json::json!({ "audioMillis": 90_400 }));
//! ```

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A unit a call's usage is counted and billed in.
///
/// Serialized in camelCase (`inputTokens`, `audioMillis`), the form quota and usage
/// payloads key their maps by.
///
/// # Example
///
/// ```
/// use adk_core::UsageUnit;
///
/// assert_eq!(UsageUnit::AudioMillis.as_str(), "audioMillis");
/// assert_eq!(serde_json::from_str::<UsageUnit>("\"images\"").unwrap(), UsageUnit::Images);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum UsageUnit {
    /// Tokens sent to a model.
    InputTokens,
    /// Tokens a model generated, including thinking tokens.
    OutputTokens,
    /// Images generated, edited or analysed.
    Images,
    /// Milliseconds of audio processed or produced.
    AudioMillis,
    /// Milliseconds of video processed or produced.
    VideoMillis,
    /// Characters of text processed, such as text synthesized to speech.
    Characters,
    /// Document pages processed.
    Pages,
    /// Requests billed as a whole.
    Requests,
}

impl UsageUnit {
    /// Every unit, in declaration order.
    pub const ALL: [UsageUnit; 8] = [
        Self::InputTokens,
        Self::OutputTokens,
        Self::Images,
        Self::AudioMillis,
        Self::VideoMillis,
        Self::Characters,
        Self::Pages,
        Self::Requests,
    ];

    /// The unit's wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InputTokens => "inputTokens",
            Self::OutputTokens => "outputTokens",
            Self::Images => "images",
            Self::AudioMillis => "audioMillis",
            Self::VideoMillis => "videoMillis",
            Self::Characters => "characters",
            Self::Pages => "pages",
            Self::Requests => "requests",
        }
    }
}

impl fmt::Display for UsageUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Usage counted per [`UsageUnit`].
///
/// Amounts add saturating; a unit with no usage is absent rather than zero, so two vectors
/// that counted the same usage compare equal. Serialized as a map keyed by the unit's wire
/// name.
///
/// # Example
///
/// ```
/// use adk_core::{UsageUnit, UsageVector};
///
/// let mut total = UsageVector::new().with(UsageUnit::Images, 2);
/// total.merge(&UsageVector::new().with(UsageUnit::Images, 1).with(UsageUnit::InputTokens, 40));
/// assert_eq!(total.get(UsageUnit::Images), 3);
/// assert_eq!(total.tokens(), 40);
/// assert_eq!(total.get(UsageUnit::Pages), 0);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UsageVector(BTreeMap<UsageUnit, u64>);

impl UsageVector {
    /// An empty vector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `amount` of `unit` and returns the vector.
    pub fn with(mut self, unit: UsageUnit, amount: u64) -> Self {
        self.add(unit, amount);
        self
    }

    /// Adds `amount` of `unit`. A zero amount leaves the vector unchanged.
    pub fn add(&mut self, unit: UsageUnit, amount: u64) {
        if amount == 0 {
            return;
        }
        let total = self.0.entry(unit).or_default();
        *total = total.saturating_add(amount);
    }

    /// Adds every amount in `other`.
    pub fn merge(&mut self, other: &UsageVector) {
        for (unit, amount) in other.iter() {
            self.add(unit, amount);
        }
    }

    /// The amount counted for `unit`, zero when none was.
    pub fn get(&self, unit: UsageUnit) -> u64 {
        self.0.get(&unit).copied().unwrap_or(0)
    }

    /// Input plus output tokens.
    pub fn tokens(&self) -> u64 {
        self.get(UsageUnit::InputTokens).saturating_add(self.get(UsageUnit::OutputTokens))
    }

    /// Returns `true` when no usage is counted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Each unit with usage and its amount, in unit order.
    pub fn iter(&self) -> impl Iterator<Item = (UsageUnit, u64)> + '_ {
        self.0.iter().map(|(unit, amount)| (*unit, *amount))
    }
}

impl FromIterator<(UsageUnit, u64)> for UsageVector {
    fn from_iter<I: IntoIterator<Item = (UsageUnit, u64)>>(iter: I) -> Self {
        let mut vector = Self::new();
        for (unit, amount) in iter {
            vector.add(unit, amount);
        }
        vector
    }
}

/// A price per unit of usage: `micro_usd` for every `per` units of `unit`.
///
/// `granularity` is the billing step: usage is billed in whole multiples of it, rounded
/// down, as a vendor billing "whole seconds, rounded down" bills audio milliseconds in
/// steps of 1,000. The cost of the billed amount rounds up to a whole micro-USD.
///
/// # Example
///
/// ```
/// use adk_core::{UnitRate, UsageUnit};
///
/// // $2.50 per 1,000 images.
/// let rate = UnitRate::new(UsageUnit::Images, 2_500_000, 1_000);
/// assert_eq!(rate.cost_micro_usd(3), 7_500);
/// assert_eq!(rate.cost_micro_usd(0), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitRate {
    /// The unit priced.
    pub unit: UsageUnit,
    /// Micro-USD charged for every [`per`](Self::per) units.
    pub micro_usd: u64,
    /// How many units [`micro_usd`](Self::micro_usd) buys. Never zero.
    pub per: u64,
    /// Usage is billed in whole multiples of this many units, rounded down. Never zero.
    pub granularity: u64,
}

impl UnitRate {
    /// `micro_usd` per `per` units of `unit`, every unit billed. A zero `per` is read as 1.
    pub const fn new(unit: UsageUnit, micro_usd: u64, per: u64) -> Self {
        Self { unit, micro_usd, per: if per == 0 { 1 } else { per }, granularity: 1 }
    }

    /// Bills usage in whole multiples of `granularity` units, rounded down. Zero is read as 1.
    pub const fn rounded_down_to(mut self, granularity: u64) -> Self {
        self.granularity = if granularity == 0 { 1 } else { granularity };
        self
    }

    /// The cost of `amount` units in micro-USD, saturating at `u64::MAX`.
    pub fn cost_micro_usd(&self, amount: u64) -> u64 {
        let granularity = self.granularity.max(1);
        let billed = u128::from(amount / granularity * granularity);
        let cost = (billed * u128::from(self.micro_usd)).div_ceil(u128::from(self.per.max(1)));
        u64::try_from(cost).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unit_round_trips_through_its_wire_name() {
        for unit in UsageUnit::ALL {
            let json = serde_json::to_value(unit).unwrap();
            assert_eq!(json, serde_json::Value::String(unit.as_str().to_string()));
            assert_eq!(serde_json::from_value::<UsageUnit>(json).unwrap(), unit);
        }
    }

    #[test]
    fn zero_amounts_are_absent_and_vectors_compare_by_usage() {
        let counted = UsageVector::new().with(UsageUnit::Images, 0).with(UsageUnit::Pages, 2);
        assert_eq!(counted, UsageVector::from_iter([(UsageUnit::Pages, 2)]));
        assert!(UsageVector::new().with(UsageUnit::Images, 0).is_empty());
    }

    #[test]
    fn amounts_saturate() {
        let mut vector = UsageVector::new().with(UsageUnit::Characters, u64::MAX);
        vector.add(UsageUnit::Characters, 5);
        assert_eq!(vector.get(UsageUnit::Characters), u64::MAX);
    }

    #[test]
    fn a_vector_deserializes_from_a_wire_map() {
        let vector: UsageVector =
            serde_json::from_value(serde_json::json!({ "images": 3, "audioMillis": 1_500 }))
                .unwrap();
        assert_eq!(
            vector,
            UsageVector::new().with(UsageUnit::Images, 3).with(UsageUnit::AudioMillis, 1_500)
        );
    }

    #[test]
    fn whole_second_billing_rounds_down_before_pricing() {
        let rate = UnitRate::new(UsageUnit::AudioMillis, 180_000, 3_600_000).rounded_down_to(1_000);
        assert_eq!(rate.cost_micro_usd(999), 0);
        assert_eq!(rate.cost_micro_usd(1_000), 50);
        assert_eq!(rate.cost_micro_usd(30_400), 1_500);
        assert_eq!(rate.cost_micro_usd(3_600_000), 180_000);
    }

    #[test]
    fn a_fractional_micro_usd_rounds_up_and_huge_amounts_saturate() {
        let rate = UnitRate::new(UsageUnit::Characters, 1, 3);
        assert_eq!(rate.cost_micro_usd(1), 1);
        assert_eq!(rate.cost_micro_usd(3), 1);
        assert_eq!(rate.cost_micro_usd(4), 2);
        assert_eq!(UnitRate::new(UsageUnit::Images, u64::MAX, 1).cost_micro_usd(2), u64::MAX);
    }
}
