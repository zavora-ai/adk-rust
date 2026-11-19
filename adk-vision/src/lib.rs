//! # adk-vision
//!
//! Local-first perception for ADK-Rust agents (spec 3.7).
//!
//! The crate is contracts first: artifact-backed images ([`ImageRef`]); regions that always
//! know their source image and the transforms between it and the model ([`Region`],
//! [`CoordFrame`], [`TransformChain`]); model manifests that pin every weight file by
//! revision and SHA-256 and name its licence ([`ModelManifest`]); and the perception traits
//! — [`Detector`], [`Segmenter`], [`DocumentReader`], [`Embedder`], [`Describer`],
//! [`Tracker`], [`VideoAnalyzer`] and [`Editor`] — each with a conformance suite.
//!
//! [`VisionRuntime`] routes each call to a backend, local first. An on-device backend is
//! handed a [`SentImage`]; a remote backend only a [`ReleasedImage`], which only the
//! [`EgressGate`] constructs after it has checked the destination, cropped, redacted,
//! decided and recorded what leaves. Every call is metered against the run's budget and
//! spend ledger, local calls at zero cost, and every answer comes back in source pixels.
//!
//! # Features
//!
//! | Feature | Contents |
//! |---------|----------|
//! | (always) | media, geometry, manifests, the traits, `VisionRuntime`, the egress gate, the deterministic `Editor` |
//! | `fetch` | `ModelFetcher`: Hugging Face downloads by revision, SHA-256 checked before load, `HF_HUB_OFFLINE` honoured |
//! | `conformance` | `conformance`: the suite every backend runs, and its test macros |
//! | `guardrail` | `ImagePrivacyGuardrail` on model calls, tool results and user input, and `PersonalTextRedactor` (OCR words matched by `adk-guardrail`'s PII patterns) |
//! | `onnx` | [`onnx`]: RF-DETR, EfficientSAM, SAM 2.1 and SigLIP 2 on ONNX Runtime, the runtime downloaded at build time |
//! | `onnx-dynamic` | The same backends against an onnxruntime library loaded from `ORT_DYLIB_PATH` |
//! | `onnx-coreml`, `onnx-cuda` | The CoreML and CUDA execution providers for the `onnx` backends |
//!
//! [`VisionToolset`] — `vision_detect`, `vision_segment`, `vision_read`, `vision_describe`,
//! `vision_crop`, `vision_redact`, `vision_annotate` — is always built: each tool runs on
//! the governed path, a remote call is held with the exact egress image as approval
//! evidence, and every call, release and edit writes its `vision.*` audit record
//! ([`audit`]).
//!
//! The other backends — OCR, mistral.rs, the cloud providers, Meta `sam-3.1`, NVIDIA NIM —
//! are features added by the workstreams that bring them.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use adk_vision::{
//!     BBox, BackendDescriptor, CoordFrame, DetectQuery, Detector, Locality, RawRegion,
//!     SentImage, SourceImage, DecodedImage, VisionCx, VisionRuntime,
//! };
//!
//! /// A detector that finds the whole image, in the pixels it was sent.
//! struct Whole(BackendDescriptor);
//!
//! #[async_trait::async_trait]
//! impl Detector for Whole {
//!     fn descriptor(&self) -> &BackendDescriptor {
//!         &self.0
//!     }
//!     async fn detect(
//!         &self,
//!         _cx: &VisionCx,
//!         image: &SentImage,
//!         _query: &DetectQuery,
//!     ) -> adk_vision::Result<Vec<RawRegion>> {
//!         let (w, h) = image.size();
//!         Ok(vec![RawRegion::boxed(BBox::new(0.0, 0.0, w as f32, h as f32), "page", 0.99)])
//!     }
//! }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! // The backend takes at most 500 px on the long edge; the runtime downscales and records it.
//! let whole = BackendDescriptor::new("whole", Locality::InProcess, CoordFrame::SentPixels)
//!     .with_limits(adk_vision::InputLimits::long_edge(500));
//! let runtime = VisionRuntime::builder().detector(Arc::new(Whole(whole))).build().unwrap();
//!
//! let page = SourceImage::from_pixels(DecodedImage::filled(2000, 1000, [255; 4])).unwrap();
//! let found = runtime
//!     .detect(&VisionCx::new(), &page, &DetectQuery::Classes(vec!["page".into()]))
//!     .await
//!     .unwrap();
//! // Back in source pixels, with the resize on record.
//! assert_eq!(found.value[0].shape.bbox(), Some(BBox::new(0.0, 0.0, 2000.0, 1000.0)));
//! assert!(!found.value[0].origin.chain.is_identity());
//! # });
//! ```

#![warn(missing_docs)]

pub mod audit;
mod backend;
mod budget;
#[cfg(feature = "conformance")]
pub mod conformance;
mod cx;
mod digest;
mod edit;
mod egress;
mod error;
#[cfg(feature = "fetch")]
mod fetch;
mod geometry;
#[cfg(feature = "guardrail")]
mod guardrail;
mod manifest;
mod media;
#[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
pub mod onnx;
mod redact;
mod runtime;
mod sent;
mod toolset;
mod tracker;
mod traits;

pub use backend::{BackendDescriptor, BackendId, InputLimits, Locality, Oversize, PriceRef};
pub use budget::{ImageTokens, TokenEstimate};
pub use cx::VisionCx;
pub use digest::Digest;
pub use edit::{DeterministicEditor, EditOp, Overlay, Rgba, apply_edit};
pub use egress::{
    DataClass, EgressAction, EgressConfig, EgressDecision, EgressEvidence, EgressGate,
    EgressPreview, EgressReceipt, EgressRecord, EgressRecorder, EgressRefusal, EgressRuleConfig,
    InMemoryEgressRecorder, RedactionClass, Redactor, ReleaseRequest, ReleasedImage, StagedEgress,
    VisionConfig,
};
pub use error::{Result, VisionError};
#[cfg(feature = "fetch")]
pub use fetch::{FetchConfig, ModelFetcher};
pub use geometry::{
    AxisOrder, BBox, CoordFrame, EscalatedFrom, EscalationReason, Keypoint, Keypoints, Letterbox,
    LetterboxGeometry, Mask, Point2, Polygon, RawRegion, Region, RegionNormalizer, RegionOrigin,
    RegionSet, ScreenSpace, Shape, Track, TrackId, Transform, TransformChain,
};
#[cfg(feature = "guardrail")]
pub use guardrail::{
    GateOutcome, ImagePrivacyGuardrail, MODEL_CONTEXT_CLASS, PersonalTextRedactor,
};
pub use manifest::{
    AcceptanceStore, FileAcceptanceStore, InMemoryAcceptanceStore, InputSpec, IoSpec,
    LicenceAcceptance, LicenceClass, LicenceGate, LicenceInfo, LicencePolicy, ManifestSet,
    ModelFile, ModelManifest, ModelSource, RuntimeSpec, VerifiedFile, VerifiedModel, VisionTask,
};
pub use media::{
    ArtifactLocator, DecodeLimits, DecodedImage, Frame, FrameMeta, FramePixels, FrameSourceId,
    ImageRef, MediaScope, MediaStore, Provenance, SourceImage, VideoRef, artifact_uri,
};
pub use redact::{PersonRedactor, REDACTION_MARGIN};
pub use runtime::{
    CallOptions, CallRecord, Edited, Escalation, Route, Trigger, VisionOutput, VisionRuntime,
    VisionRuntimeBuilder,
};
pub use sent::{SentImage, VisionInput};
pub use toolset::{
    EgressApproval, VISION_EGRESS_POLICY_REF, VISION_TOOL_NAMES, VisionTool, VisionToolKind,
    VisionToolset, VisionToolsetBuilder,
};
pub use tracker::GreedyIouTracker;
pub use traits::{
    Block, BlockKind, DescribeRequest, DescribeSpec, Describer, Description, DetectQuery, Detector,
    DocumentPage, DocumentReader, Editor, Embedder, EmbeddingSpace, ImageRegion, Metric,
    ReadOptions, Sampling, SegmentPrompt, Segmenter, TableCell, Tracker, VideoAnalysis,
    VideoAnalyzer, VideoEvent, VideoInput, VideoRequest, Word,
};
