//! How audio providers bill, for metering their calls against the current run.
//!
//! A cloud provider reports the vendor and model its calls are billed under
//! ([`SttProvider::billing`](crate::SttProvider::billing),
//! [`TtsProvider::billing`](crate::TtsProvider::billing)); a local model bills nothing and
//! reports `None`. [`TranscribeTool`](crate::TranscribeTool) and
//! [`SpeakTool`](crate::SpeakTool) meter every billed call through
//! [`adk_core::meter_usage`], against the run the tool call belongs to:
//!
//! | Call | Unit |
//! |------|------|
//! | Transcription | [`UsageUnit::AudioMillis`] of the audio sent |
//! | Synthesis | [`UsageUnit::Characters`] of the text sent |
//!
//! The bundled cloud providers carry no per-unit price, so their cost is unknown: under a
//! run budget's cost cap or a spend-ledger limit the call is refused before it is sent
//! (opt out of the budget check with `RunBudget::allow_unpriced_models`), and without a cap
//! the ledger commits the run's per-call spend estimate. A provider built with a price
//! ([`AudioBilling::with_rate`]) is held and committed at that price.
//!
//! # Example
//!
//! ```
//! use adk_audio::AudioBilling;
//! use adk_core::{UnitRate, UsageUnit, UsageVector};
//!
//! // $0.18 per audio hour, billed in whole seconds rounded down.
//! let billing = AudioBilling::new("muse", "muse-voice-transcribe-1.0").with_rate(
//!     UnitRate::new(UsageUnit::AudioMillis, 180_000, 3_600_000).rounded_down_to(1_000),
//! );
//! let (call, usage) = billing.meter(UsageVector::new().with(UsageUnit::AudioMillis, 90_000));
//! assert_eq!(call.max_cost_micro_usd, Some(4_500));
//! assert_eq!(usage.cost_micro_usd, Some(4_500));
//! ```

use adk_core::{MeteredUsage, UnitRate, UsageCall, UsageVector};

#[cfg(doc)]
use adk_core::UsageUnit;

/// The vendor and model an audio provider's calls are billed under, and their price when
/// known.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AudioBilling {
    /// The spend key vendor, such as `deepgram` or `openai`.
    pub vendor: String,
    /// The model billed, named in budget errors and logs.
    pub model: String,
    /// The price per unit, or `None` when unknown.
    pub rate: Option<UnitRate>,
}

impl AudioBilling {
    /// Calls to `model` billed by `vendor`, at an unknown price.
    pub fn new(vendor: impl Into<String>, model: impl Into<String>) -> Self {
        Self { vendor: vendor.into(), model: model.into(), rate: None }
    }

    /// Prices every call at `rate`.
    pub fn with_rate(mut self, rate: UnitRate) -> Self {
        self.rate = Some(rate);
        self
    }

    /// The metered call and its usage for a call that sends `units`, priced on the rate's
    /// unit. The input decides an audio call's billed amount, so the call's bound and its
    /// cost are the same.
    pub fn meter(&self, units: UsageVector) -> (UsageCall, MeteredUsage) {
        let cost = self.rate.map(|rate| rate.cost_micro_usd(units.get(rate.unit)));
        let call = UsageCall::new(self.vendor.clone(), self.model.clone());
        let usage = MeteredUsage::new(units);
        match cost {
            Some(cost) => (call.with_max_cost_micro_usd(cost), usage.with_cost_micro_usd(cost)),
            None => (call, usage),
        }
    }
}
