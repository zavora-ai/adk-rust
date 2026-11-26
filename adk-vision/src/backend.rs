//! [`BackendDescriptor`]: what a backend is, where it runs, how it answers and what it costs.

use std::fmt;

use adk_core::{UnitRate, UsageUnit};
use serde::{Deserialize, Serialize};

use crate::geometry::CoordFrame;

/// A backend's stable identifier, such as `rf-detr-m` or `sam-3.1`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendId(String);

impl BackendId {
    /// A backend identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for BackendId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

/// Where a backend's computation happens, which decides whether an image leaves the
/// machine.
///
/// Mirrors `adk_core::DataLocality` (spec 3.7 X6: `InProcess` for `adk-mistralrs`,
/// `Loopback` for a client at a loopback address, `Remote` otherwise), with the hosts a
/// remote backend sends to, which the egress policy checks.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Locality {
    /// In this process.
    InProcess,
    /// A server on this machine, reached over loopback.
    Loopback,
    /// Off the machine: images reach it only through the egress gate.
    Remote {
        /// The hosts the backend sends to; the egress policy checks each.
        hosts: Vec<String>,
    },
}

impl Locality {
    /// `true` for [`Remote`](Self::Remote).
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }
}

/// How a priced backend is billed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum PriceRef {
    /// Billed per unit by the runtime: `sam-3.1` at $2.50 per 1,000 images.
    PerUnit {
        /// The spend-key vendor, such as `meta`.
        vendor: String,
        /// The model, as the catalog names it.
        model: String,
        /// The rate.
        rate: UnitRate,
        /// The fewest units one request is billed for (`sam-3.1` video bills 50 frames).
        /// Moves into the rate as `UnitRate::with_minimum_per_request` once adk-core has it
        /// (spec 3.7 X3).
        #[serde(default)]
        min_units_per_request: u64,
    },
    /// Billed per token by the model call itself, which meters its tokens through the
    /// current `ModelCallScope`; the runtime records the call's images at zero cost.
    Tokens {
        /// The spend-key vendor.
        vendor: String,
        /// The model.
        model: String,
    },
}

impl PriceRef {
    /// The spend-key vendor.
    pub fn vendor(&self) -> &str {
        match self {
            Self::PerUnit { vendor, .. } | Self::Tokens { vendor, .. } => vendor,
        }
    }

    /// The model.
    pub fn model(&self) -> &str {
        match self {
            Self::PerUnit { model, .. } | Self::Tokens { model, .. } => model,
        }
    }

    /// The cost of a request that uses `amount` units of the priced unit, in micro-USD, or
    /// `None` for token-billed backends.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::{UnitRate, UsageUnit};
    /// use adk_vision::PriceRef;
    ///
    /// let sam = PriceRef::PerUnit {
    ///     vendor: "meta".into(),
    ///     model: "sam-3.1".into(),
    ///     rate: UnitRate::new(UsageUnit::Images, 2_500_000, 1_000),
    ///     min_units_per_request: 0,
    /// };
    /// assert_eq!(sam.request_cost(1), Some(2_500));
    /// ```
    pub fn request_cost(&self, amount: u64) -> Option<u64> {
        match self {
            Self::PerUnit { rate, min_units_per_request, .. } => {
                Some(rate.cost_micro_usd(amount.max(*min_units_per_request)))
            }
            Self::Tokens { .. } => None,
        }
    }

    /// The unit a per-unit price bills in.
    pub fn unit(&self) -> Option<UsageUnit> {
        match self {
            Self::PerUnit { rate, .. } => Some(rate.unit),
            Self::Tokens { .. } => None,
        }
    }
}

/// What a backend does with an image larger than its limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Oversize {
    /// The runtime downscales it to the limit and records the resize in the transform chain.
    Downscale,
    /// The runtime refuses it, naming the limit.
    Refuse,
}

/// The largest image a backend takes.
///
/// # Example
///
/// ```
/// use adk_vision::InputLimits;
///
/// // Claude 4.7 and later: 2,576 px on the long edge.
/// let limits = InputLimits::long_edge(2576);
/// assert_eq!(limits.fit((5152, 3000)), Some((2576, 1500)));
/// assert_eq!(limits.fit((1000, 800)), None);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputLimits {
    /// Longest side in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_long_edge: Option<u32>,
    /// Total pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pixels: Option<u64>,
    /// What happens to a larger image.
    pub oversize: Oversize,
}

impl InputLimits {
    /// No limit.
    pub const fn unlimited() -> Self {
        Self { max_long_edge: None, max_pixels: None, oversize: Oversize::Downscale }
    }

    /// At most `pixels` on the long edge, larger images downscaled.
    pub const fn long_edge(pixels: u32) -> Self {
        Self { max_long_edge: Some(pixels), max_pixels: None, oversize: Oversize::Downscale }
    }

    /// Sets the total-pixel limit.
    pub const fn with_max_pixels(mut self, pixels: u64) -> Self {
        self.max_pixels = Some(pixels);
        self
    }

    /// Refuses larger images instead of downscaling them.
    pub const fn refusing(mut self) -> Self {
        self.oversize = Oversize::Refuse;
        self
    }

    /// `true` when an image of `size` is within the limits.
    pub fn admits(&self, size: (u32, u32)) -> bool {
        let long = size.0.max(size.1);
        let pixels = u64::from(size.0) * u64::from(size.1);
        self.max_long_edge.is_none_or(|max| long <= max)
            && self.max_pixels.is_none_or(|max| pixels <= max)
    }

    /// The size an image of `size` is downscaled to, keeping its aspect ratio, or `None`
    /// when it is within the limits.
    pub fn fit(&self, size: (u32, u32)) -> Option<(u32, u32)> {
        if self.admits(size) {
            return None;
        }
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        let mut scale = 1.0f64;
        if let Some(max) = self.max_long_edge {
            scale = scale.min(f64::from(max) / w.max(h));
        }
        if let Some(max) = self.max_pixels {
            scale = scale.min((max as f64 / (w * h)).sqrt());
        }
        let mut fitted = (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1));
        // Rounding may leave a side or the pixel count a hair over; step down until it fits.
        while !self.admits(fitted) && (fitted.0 > 1 || fitted.1 > 1) {
            if fitted.0 >= fitted.1 {
                fitted.0 -= 1;
                fitted.1 = ((f64::from(fitted.0) * h / w).round() as u32).max(1);
            } else {
                fitted.1 -= 1;
                fitted.0 = ((f64::from(fitted.1) * w / h).round() as u32).max(1);
            }
        }
        Some(fitted)
    }

    /// The limits in words, for error messages.
    pub fn describe(&self) -> String {
        match (self.max_long_edge, self.max_pixels) {
            (Some(edge), Some(px)) => format!("{edge} px on the long edge and {px} pixels"),
            (Some(edge), None) => format!("{edge} px on the long edge"),
            (None, Some(px)) => format!("{px} pixels"),
            (None, None) => "any size".to_string(),
        }
    }
}

impl Default for InputLimits {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// What a backend is, where it runs, the frame it answers in, what it costs and what it
/// takes.
///
/// # Example
///
/// ```
/// use adk_vision::{BackendDescriptor, CoordFrame, InputLimits, Letterbox, Locality};
///
/// let rf_detr = BackendDescriptor::new(
///     "rf-detr-m",
///     Locality::InProcess,
///     CoordFrame::ModelInput { width: 576, height: 576, letterbox: Letterbox::Center },
/// )
/// .with_manifest("rf-detr-m");
/// assert!(!rf_detr.locality.is_remote());
/// assert!(rf_detr.price.is_none());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendDescriptor {
    /// The backend's identifier.
    pub id: BackendId,
    /// Where it runs.
    pub locality: Locality,
    /// The providers a remote backend sends to, by the id their responses carry in
    /// `LlmResponse::provider` (`anthropic`, `gemini`, `bedrock`), which `provider` egress
    /// rules match; empty when unknown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// The coordinate frame its regions are in.
    pub frame: CoordFrame,
    /// How it is billed; `None` for local backends, which record units at zero cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<PriceRef>,
    /// The model manifest entry it runs, for model-backed backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// The largest image it takes.
    #[serde(default)]
    pub limits: InputLimits,
    /// How a token-billed backend's cost is estimated before its request, so the run's
    /// budget can refuse it before any image leaves (spec 3.7 §7.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<crate::budget::TokenEstimate>,
    /// The most open-vocabulary concepts one request takes (`sam-3.1` takes one), so a
    /// call with more is held and metered as one request per that many concepts; `None` for
    /// no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concepts_per_request: Option<u32>,
}

impl BackendDescriptor {
    /// A descriptor with no price, no manifest and no input limit.
    pub fn new(id: impl Into<String>, locality: Locality, frame: CoordFrame) -> Self {
        Self {
            id: BackendId::new(id),
            locality,
            providers: Vec::new(),
            frame,
            price: None,
            manifest: None,
            limits: InputLimits::unlimited(),
            token_estimate: None,
            concepts_per_request: None,
        }
    }

    /// Names a provider the backend sends to, for `provider` egress rules.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{BackendDescriptor, CoordFrame, Locality};
    ///
    /// let sam = BackendDescriptor::new(
    ///     "sam-3.1",
    ///     Locality::Remote { hosts: vec!["api.meta.ai".into()] },
    ///     CoordFrame::SentPixels,
    /// )
    /// .with_provider("muse");
    /// assert_eq!(sam.providers, ["muse"]);
    /// ```
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        let provider = provider.into();
        if !self.providers.contains(&provider) {
            self.providers.push(provider);
        }
        self
    }

    /// Sets the preflight estimate of a token-billed backend.
    pub fn with_token_estimate(mut self, estimate: crate::budget::TokenEstimate) -> Self {
        self.token_estimate = Some(estimate);
        self
    }

    /// Sets the price.
    pub fn with_price(mut self, price: PriceRef) -> Self {
        self.price = Some(price);
        self
    }

    /// Names the model manifest entry.
    pub fn with_manifest(mut self, manifest: impl Into<String>) -> Self {
        self.manifest = Some(manifest.into());
        self
    }

    /// Sets the input limits.
    pub fn with_limits(mut self, limits: InputLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Takes at most `concepts` open-vocabulary concepts per request.
    pub fn with_concepts_per_request(mut self, concepts: u32) -> Self {
        self.concepts_per_request = Some(concepts.max(1));
        self
    }

    /// The requests a call with `concepts` concepts and, separately, other prompts makes:
    /// one per [`concepts_per_request`](Self::concepts_per_request) concepts, and one for
    /// the other prompts; always 1 for a backend with no limit.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{BackendDescriptor, CoordFrame, Locality};
    ///
    /// let sam = BackendDescriptor::new("sam-3.1", Locality::Remote { hosts: vec![] }, CoordFrame::SentPixels)
    ///     .with_concepts_per_request(1);
    /// assert_eq!(sam.requests_for(5, false), 5);
    /// assert_eq!(sam.requests_for(2, true), 3);
    /// ```
    pub fn requests_for(&self, concepts: usize, other_prompts: bool) -> u64 {
        match self.concepts_per_request {
            Some(per) => {
                let per = per.max(1) as usize;
                (concepts.div_ceil(per) + usize::from(other_prompts)).max(1) as u64
            }
            None => 1,
        }
    }
}
