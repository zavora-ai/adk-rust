//! The conformance suite every vision backend runs (spec 3.7 §7.18), feature `conformance`.
//!
//! | Trait | Test macro | Cases |
//! |-------|------------|-------|
//! | [`Detector`] | [`detector_conformance_tests!`](crate::detector_conformance_tests) | coordinates, usage, cancellation, kill switch, egress, oversized input, unreleased images |
//! | [`Segmenter`] | [`segmenter_conformance_tests!`](crate::segmenter_conformance_tests) | the same |
//! | [`DocumentReader`] | [`document_reader_conformance_tests!`](crate::document_reader_conformance_tests) | the same |
//! | [`Describer`] | [`describer_conformance_tests!`](crate::describer_conformance_tests) | the same, coordinates when the backend returns regions |
//! | [`Embedder`] | [`embedder_conformance_tests!`](crate::embedder_conformance_tests) | usage, cancellation, kill switch, egress, oversized input, unreleased images, dimensions |
//! | [`Editor`] | [`editor_conformance_tests!`](crate::editor_conformance_tests) | the same, provenance |
//! | [`Tracker`](crate::Tracker) | [`tracker_conformance_tests!`](crate::tracker_conformance_tests) | stable identifiers, new objects, reset |
//! | [`VideoAnalyzer`](crate::VideoAnalyzer) | [`video_analyzer_conformance_tests!`](crate::video_analyzer_conformance_tests) | usage, egress, event times and track regions, unsupported requests, cancellation, kill switch, frame cap, unreleased media |
//!
//! Every case runs the backend through a [`VisionRuntime`](crate::VisionRuntime), so the image it sees, the
//! metering, the egress gate and the mapping to source pixels are the ones production uses:
//!
//! - **Coordinates.** Regions are in source pixels within the fixture's tolerance, belong to
//!   the fixture image, lie inside it, and carry the backend, its frame and the transform
//!   chain (a resize recorded exactly when the backend's limit required one).
//! - **Usage.** Every call records its images as units, at zero cost for a local backend
//!   and at its unit price for a per-unit one.
//! - **Cancellation and the kill switch.** A cancelled call ends with `Cancelled`, within
//!   one call for a slow backend; a frozen kill switch refuses before the backend runs.
//! - **Egress.** A local backend leaves the egress recorder empty; a remote backend's every
//!   image is a recorded release. A remote backend without an egress gate is refused, and
//!   handing it an unreleased image does not compile (see [`SentImage`](crate::SentImage)).
//! - **Oversized input.** Refused with the limit in the message, or downscaled with the
//!   resize recorded, as the descriptor says.
//! - **Errors.** A rate-limited backend maps to `RateLimited` with a retry hint.
//! - **Video.** A video call records its frames as `UsageUnit::Frames`; event times are
//!   milliseconds from the start of the video, ordered and inside it; track regions are in
//!   source pixels of the clip or their frame; an unsupported request, a cancelled call, the
//!   kill switch and a frame cap each refuse before anything leaves.
//!
//! The macros expand to `#[tokio::test]`s on a multi-threaded runtime, so the calling crate
//! needs `tokio` with the `macros` and `rt-multi-thread` features. Invoke each macro in its
//! own module.
//!
//! # Example
//!
//! ```rust,ignore
//! mod my_detector {
//!     use std::sync::Arc;
//!     use adk_vision::conformance::DetectorTarget;
//!
//!     adk_vision::detector_conformance_tests! {
//!         backend = DetectorTarget::Local(Arc::new(my_crate::MyDetector::new())),
//!     }
//! }
//! ```

mod checks;
mod macros;
mod probe;
mod video;

pub use checks::*;
pub use probe::{AnyProbe, Expected, Fixture, Harness, ProbeOutput, Target};
pub use video::*;

use crate::egress::ReleasedImage;
use crate::traits::{Describer, Detector, DocumentReader, Editor, Embedder, Segmenter};

/// A detector under test.
pub type DetectorTarget = Target<dyn Detector, dyn Detector<ReleasedImage>>;
/// A segmenter under test.
pub type SegmenterTarget = Target<dyn Segmenter, dyn Segmenter<ReleasedImage>>;
/// A document reader under test.
pub type ReaderTarget = Target<dyn DocumentReader, dyn DocumentReader<ReleasedImage>>;
/// An embedder under test.
pub type EmbedderTarget = Target<dyn Embedder, dyn Embedder<ReleasedImage>>;
/// A describer under test.
pub type DescriberTarget = Target<dyn Describer, dyn Describer<ReleasedImage>>;
/// An editor under test.
pub type EditorTarget = Target<dyn Editor, dyn Editor<ReleasedImage>>;
