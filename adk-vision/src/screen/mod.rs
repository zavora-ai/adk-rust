//! The Screen copilot's grounding (spec 3.7 §7.14): set-of-marks candidates on a
//! screenshot, the target a chosen mark resolves to in logical points, screenshots fitted
//! to a model's image limits with the scale kept, and previewed plans approved once.
//!
//! | Piece | What it does |
//! |---|---|
//! | [`MarkSet`] | Numbered candidates from `browser_elements`, local OCR words or a model's point, drawn by `vision_annotate` at the device pixel ratio |
//! | [`GroundedTarget`] | A chosen mark: region in pixels and points, click point, and the screenshot digest; a `browser_click` selector for a browser element; `TargetEvidence` with the `computer-use` feature |
//! | [`ScreenshotLimits`], [`ScreenshotScale`], [`fit_screenshot`] | Resize before sending, as the provider would, and map coordinates back |
//! | [`FittedActuator`] | A `ComputerActuator` whose screenshots fit the model and whose clicks and `zoom` regions are scaled back |
//! | [`ScreenPlan`], [`PreviewedTarget`] | One approval per plan of up to N actions in one window; each action grounded again and checked before it runs (Q8) |

mod fit;
mod marks;
mod plan;

#[cfg(feature = "cloud")]
pub(crate) use fit::claude_high_tier;
pub use fit::{FittedActuator, ScreenshotLimits, ScreenshotScale, fit_screenshot};
pub use marks::{
    BrowserElement, BrowserElements, Candidate, CandidateSource, CssRect, GroundedTarget, MarkSet,
};
pub use plan::{Deviation, PlanRefusal, PlanStep, PreviewedTarget, ScreenPlan};
