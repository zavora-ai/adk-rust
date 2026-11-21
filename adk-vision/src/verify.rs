//! The verifier loop (spec 3.7 §7.14, "Verification"): after an action or a claim, a fresh
//! observation is compared with the one before it, region by region.
//!
//! | Step | What happens |
//! |------|--------------|
//! | Observe | The [`Observer`] takes the observation before the action |
//! | Act | The [`VerifiedAction`] acts and states the [`ExpectedChange`] |
//! | Re-observe | A fresh observation is taken |
//! | Check | [`check_change`] compares the two: every expected region appeared or disappeared, and nothing else did |
//! | Re-ground | A failed check is handed back to the action once, which grounds its target again and acts again |
//! | Hold | A second failed check holds for a person: [`VerificationHold::governed_call`] puts the before and after frames on one card |
//!
//! The loop is generic: a screen copilot's observer takes screenshots, an Inspector's reads
//! a camera, and a claim with no action checks one observation against what it states.

use std::sync::Arc;

use adk_core::egress::ToolEgress;
use adk_core::{Evidence, GovernedCall, ImageEvidence, ToolEffect};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::Result;
use crate::geometry::{BBox, Region, RegionSet};
use crate::media::ImageRef;

/// The governed call a failed verification is held as.
///
/// # Example
///
/// ```
/// assert_eq!(adk_vision::VERIFICATION_TOOL, "vision_verification_failed");
/// ```
pub const VERIFICATION_TOOL: &str = "vision_verification_failed";

/// One fresh look: the regions found and, when it was stored, the frame they were found in.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Observation {
    /// The regions, in the frame's source pixels.
    pub regions: RegionSet,
    /// The stored frame, for a card.
    pub frame: Option<ImageRef>,
}

impl Observation {
    /// An observation of `regions`.
    pub fn new(regions: RegionSet) -> Self {
        Self { regions, frame: None }
    }

    /// The stored frame the regions were found in.
    #[must_use]
    pub fn with_frame(mut self, frame: ImageRef) -> Self {
        self.frame = Some(frame);
        self
    }
}

/// Takes fresh observations.
#[async_trait]
pub trait Observer: Send + Sync {
    /// Observes now.
    ///
    /// # Errors
    ///
    /// The source's or the perception backend's error.
    async fn observe(&self, cx: &VisionCx) -> Result<Observation>;
}

/// A region the action expects to appear or disappear: a label, and optionally where.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RegionExpectation {
    /// The class.
    pub label: String,
    /// Where, in source pixels; anywhere when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<BBox>,
}

impl RegionExpectation {
    /// A region of `label` anywhere.
    pub fn label(label: impl Into<String>) -> Self {
        Self { label: label.into(), within: None }
    }

    /// Only inside `bbox`.
    #[must_use]
    pub fn within(mut self, bbox: BBox) -> Self {
        self.within = Some(bbox);
        self
    }

    fn matches(&self, region: &Region) -> bool {
        region.label.as_deref() == Some(self.label.as_str())
            && match (self.within, region.shape.bbox()) {
                (None, _) => true,
                (Some(area), Some(bbox)) => {
                    let [cx, cy] = bbox.center();
                    cx >= area.x0 && cx <= area.x1 && cy >= area.y0 && cy <= area.y1
                }
                (Some(_), None) => false,
            }
    }
}

/// What an action should change: regions that appear, regions that disappear; everything
/// else stays.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ExpectedChange {
    /// Regions that should appear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub appear: Vec<RegionExpectation>,
    /// Regions that should disappear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disappear: Vec<RegionExpectation>,
    /// The overlap at which a region before and one after count as the same; 0.5 by default.
    #[serde(default = "default_iou")]
    pub same_iou: f32,
}

fn default_iou() -> f32 {
    0.5
}

impl ExpectedChange {
    /// Nothing should change.
    pub fn none() -> Self {
        Self { same_iou: default_iou(), ..Self::default() }
    }

    /// `expectation` should appear.
    #[must_use]
    pub fn appears(mut self, expectation: RegionExpectation) -> Self {
        self.appear.push(expectation);
        self
    }

    /// `expectation` should disappear.
    #[must_use]
    pub fn disappears(mut self, expectation: RegionExpectation) -> Self {
        self.disappear.push(expectation);
        self
    }
}

/// What changed between two observations.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RegionDiff {
    /// Regions after with no counterpart before.
    pub appeared: Vec<Region>,
    /// Regions before with no counterpart after.
    pub disappeared: Vec<Region>,
}

/// Pairs regions of the same label across two observations by overlap.
///
/// # Example
///
/// ```
/// use adk_vision::{
///     BBox, BackendId, CoordFrame, Digest, RawRegion, RegionNormalizer, RegionSet,
///     TransformChain, diff_regions,
/// };
///
/// fn set(boxes: &[(f32, &str)]) -> RegionSet {
///     let digest = Digest::of(b"frame");
///     let normalizer = RegionNormalizer::new(
///         digest,
///         TransformChain::new(100, 100),
///         CoordFrame::SentPixels,
///         BackendId::new("test"),
///     );
///     let raw = boxes
///         .iter()
///         .map(|(x, label)| RawRegion::boxed(BBox::new(*x, 10.0, x + 10.0, 20.0), *label, 0.9))
///         .collect();
///     RegionSet::new(digest, 100, 100, normalizer.regions(raw).unwrap())
/// }
///
/// let before = set(&[(10.0, "button")]);
/// let after = set(&[(10.0, "button"), (50.0, "dialog")]);
/// let diff = diff_regions(&before, &after, 0.5);
/// assert_eq!(diff.appeared.len(), 1);
/// assert!(diff.disappeared.is_empty());
/// ```
pub fn diff_regions(before: &RegionSet, after: &RegionSet, same_iou: f32) -> RegionDiff {
    let same = |a: &Region, b: &Region| {
        a.label == b.label
            && match (a.shape.bbox(), b.shape.bbox()) {
                (Some(x), Some(y)) => x.iou(&y) >= same_iou,
                _ => false,
            }
    };
    let mut matched = vec![false; after.regions.len()];
    let mut disappeared = Vec::new();
    for old in &before.regions {
        match after.regions.iter().enumerate().position(|(i, new)| !matched[i] && same(old, new)) {
            Some(index) => matched[index] = true,
            None => disappeared.push(old.clone()),
        }
    }
    let appeared = after
        .regions
        .iter()
        .zip(matched)
        .filter(|(_, matched)| !matched)
        .map(|(region, _)| region.clone())
        .collect();
    RegionDiff { appeared, disappeared }
}

/// The outcome of one check.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VerificationReport {
    /// Whether every expected change happened and nothing else changed.
    pub passed: bool,
    /// Expected changes that did not happen, described.
    pub missing: Vec<String>,
    /// Changes nobody expected.
    pub unexpected: Vec<Region>,
    /// What changed.
    pub diff: RegionDiff,
    /// The frame before.
    pub before: Digest,
    /// The frame after.
    pub after: Digest,
}

/// Checks `expected` against what changed from `before` to `after`.
///
/// # Example
///
/// ```rust,ignore
/// let report = check_change(&before, &after, &ExpectedChange::none().appears(RegionExpectation::label("saved")));
/// assert!(report.passed);
/// ```
pub fn check_change(
    before: &RegionSet,
    after: &RegionSet,
    expected: &ExpectedChange,
) -> VerificationReport {
    let diff = diff_regions(before, after, expected.same_iou);
    let mut missing = Vec::new();
    let mut appeared: Vec<Option<&Region>> = diff.appeared.iter().map(Some).collect();
    let mut disappeared: Vec<Option<&Region>> = diff.disappeared.iter().map(Some).collect();
    for (want, pool, verb) in [
        (&expected.appear, &mut appeared, "appear"),
        (&expected.disappear, &mut disappeared, "disappear"),
    ] {
        for expectation in want {
            match pool.iter_mut().find(|r| r.is_some_and(|r| expectation.matches(r))) {
                Some(slot) => *slot = None,
                None => missing.push(format!(
                    "`{}` did not {verb}{}",
                    expectation.label,
                    expectation
                        .within
                        .map(|b| format!(" in [{}, {}, {}, {}]", b.x0, b.y0, b.x1, b.y1))
                        .unwrap_or_default()
                )),
            }
        }
    }
    let unexpected: Vec<Region> =
        appeared.into_iter().chain(disappeared).flatten().cloned().collect();
    VerificationReport {
        passed: missing.is_empty() && unexpected.is_empty(),
        missing,
        unexpected,
        diff,
        before: before.image,
        after: after.image,
    }
}

/// An action, or a claim, the loop verifies.
#[async_trait]
pub trait VerifiedAction: Send + Sync {
    /// Acts and says what should change. `failed` is `None` on the first attempt and the
    /// failed check on the re-grounded one, which grounds the target again before acting.
    ///
    /// # Errors
    ///
    /// The action's error, which ends the loop.
    async fn act(
        &self,
        cx: &VisionCx,
        failed: Option<&VerificationReport>,
    ) -> Result<ExpectedChange>;
}

/// How a verified action ended.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum LoopOutcome {
    /// The check passed, after `attempts` attempts.
    Verified {
        /// 1, or 2 when re-grounding fixed it.
        attempts: u32,
        /// The passing check.
        report: VerificationReport,
    },
    /// Both checks failed; a person decides.
    Held(VerificationHold),
}

/// A verification that failed twice, for a person.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct VerificationHold {
    /// Both failed checks.
    pub reports: Vec<VerificationReport>,
    /// The frames before and after the last attempt, when they were stored.
    pub frames: Vec<ImageRef>,
}

impl VerificationHold {
    /// The governed call to hold for a person: the failed checks as arguments and the
    /// stored frames, with what changed drawn on the last, as evidence.
    pub fn governed_call(&self, function_call_id: &str) -> GovernedCall {
        let evidence = self
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                let mut image = ImageEvidence::new(
                    frame.digest.to_string(),
                    frame.artifact.clone(),
                    frame.mime_type.clone(),
                    frame.width,
                    frame.height,
                )
                .with_caption(if index == 0 {
                    "Before the action"
                } else {
                    "After the action"
                });
                if index + 1 == self.frames.len()
                    && let Some(report) = self.reports.last()
                {
                    let regions = RegionSet::new(
                        frame.digest,
                        frame.width,
                        frame.height,
                        report.unexpected.clone(),
                    );
                    image = image.with_regions(serde_json::to_value(regions).unwrap_or_default());
                }
                Evidence::from(image)
            })
            .collect::<Vec<_>>();
        let mut call = GovernedCall::new(
            VERIFICATION_TOOL,
            function_call_id,
            json!({ "reports": self.reports }),
        )
        .with_effect(ToolEffect::Idempotent)
        .with_egress(ToolEgress::None)
        .with_evidence(evidence);
        call.requires_confirmation = true;
        call
    }
}

/// Observe, act, re-observe, check; re-ground once; then hold (spec 3.7 §7.14).
///
/// # Example
///
/// ```rust,ignore
/// let outcome = VerifierLoop::new(screen_observer).run(&cx, &click_save).await?;
/// if let LoopOutcome::Held(hold) = outcome {
///     adk_core::authorize_tool_call(&gate, hold.governed_call("verify-1")).await?;
/// }
/// ```
pub struct VerifierLoop {
    observer: Arc<dyn Observer>,
    regrounds: u32,
}

impl VerifierLoop {
    /// A loop observing through `observer` that re-grounds once.
    pub fn new(observer: Arc<dyn Observer>) -> Self {
        Self { observer, regrounds: 1 }
    }

    /// Runs `action` under the loop.
    ///
    /// # Errors
    ///
    /// The observer's and the action's errors.
    pub async fn run(&self, cx: &VisionCx, action: &dyn VerifiedAction) -> Result<LoopOutcome> {
        let mut before = self.observer.observe(cx).await?;
        let mut reports: Vec<VerificationReport> = Vec::new();
        let mut frames = Vec::new();
        for attempt in 0..=self.regrounds {
            let expected = action.act(cx, reports.last()).await?;
            let after = self.observer.observe(cx).await?;
            let report = check_change(&before.regions, &after.regions, &expected);
            tracing::debug!(
                vision.attempt = attempt + 1,
                vision.passed = report.passed,
                vision.unexpected = report.unexpected.len(),
                "verification checked"
            );
            if report.passed {
                return Ok(LoopOutcome::Verified { attempts: attempt + 1, report });
            }
            frames = before.frame.iter().chain(after.frame.iter()).cloned().collect();
            reports.push(report);
            before = after;
        }
        tracing::info!(vision.reports = reports.len(), "verification failed; held for a person");
        Ok(LoopOutcome::Held(VerificationHold { reports, frames }))
    }
}
