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
//! | (always) | media, geometry, manifests, the traits, `VisionRuntime`, the egress gate, the deterministic `Editor`, watch conditions ([`WatchEvaluator`]), calibration and measurement ([`Calibration`]), the verifier loop ([`VerifierLoop`]) |
//! | `fetch` | `ModelFetcher`: Hugging Face downloads by revision, SHA-256 checked before load, `HF_HUB_OFFLINE` honoured |
//! | `conformance` | `conformance`: the suite every backend runs, and its test macros |
//! | `eval` | `eval`: any runtime task as an `adk-eval` agent, in process or behind `cargo adk eval --agent-cmd` |
//! | `guardrail` | `ImagePrivacyGuardrail` on model calls, tool results and user input, and `PersonalTextRedactor` (OCR words matched by `adk-guardrail`'s PII patterns) |
//! | `onnx` | `onnx`: RF-DETR, EfficientSAM, SAM 2.1 and SigLIP 2 on ONNX Runtime, the runtime downloaded at build time |
//! | `onnx-dynamic` | The same backends against an onnxruntime library loaded from `ORT_DYLIB_PATH` |
//! | `onnx-coreml`, `onnx-cuda` | The CoreML and CUDA execution providers for the `onnx` backends |
//! | `cloud` | `LlmDescriber` over any `adk_core::Llm` that takes images, with the Gemini, Claude and 0–1000 region adapters; `FramesAnalyzer`, questions over sampled frames on the same models |
//! | `nim` | NVIDIA NIM: vision-language models over the OpenAI-compatible client, and `NimDocumentReader` for NVIDIA's OCR, page, table and chart endpoints, remote (implies `cloud`) |
//! | `sam` | The Meta `sam-3.1` client: `SamSegmenter`, its stream parser and mask placement; `SamVideoTracker`, masks with stable object identifiers over a clip |
//! | `sam-codec` | `LicensedMaskDecoder`: `sam-3.1` masks decoded in process by `adk-vision-sam-codec`, the port of Meta's codec under the SAM License, after a person records acceptance (implies `sam`) |
//! | `ocr` | `PpOcrReader`: PP-OCRv6 text lines and words through `oar-ocr` on ONNX Runtime |
//! | `ocr-vl` | `PaddleOcrVlReader`: PaddleOCR-VL-1.6 with PP-DocLayoutV3 through `oar-ocr-vl` on Candle |
//! | `ocr-vl-metal` | `ocr-vl` on Apple GPUs |
//! | `mistralrs` | `mistralrs`: `LlmDescriber::mistralrs` loads a verified Qwen3-VL (or another mistral.rs vision model) in this process; regions in Qwen3-VL's own 0–1000 frame (implies `cloud`, `fetch`) |
//! | `mistralrs-metal`, `mistralrs-cuda` | `mistralrs` on Apple GPUs or CUDA |
//! | `stream` | `stream`: `FrameSource`, the sampler, the frame governor, frame retention and the stream pipeline |
//! | `rtsp`, `ffmpeg`, `screen`, `browser`, `surface-camera` | The frame sources: `RtspSource`, `FileSource` (and H.264/H.265 through a user-installed ffmpeg), `ScreenSource`, `BrowserSource`, `SurfaceCameraSource` |
//! | `stream-fixtures` | `stream::fixtures`: the generated conveyor clip and an in-process RTSP test server |
//! | `track` | `ByteTracker`: ByteTrack through `trackforge`, anonymous tracks with stable identifiers across frames |
//! | `gemini-video` | `GeminiVideoAnalyzer`: Gemini native video, through the File API above 20 MB, at an explicit frame rate and media resolution (implies `cloud`) |
//! | `memory` | `memory`: `VisualMemory`, image embeddings as `adk-rag` chunks in any vector store with one embedding space per collection, text and example-image queries, `SiglipTextEmbedding` for `adk-rag` pipelines, captions with artifact references in `adk-memory`, erasure by source; the S16 week corpus |
//! | `team` | `team`: the perceiver, reasoner and verifier roles and `VisionTeam`, which escalates a disagreement to a person on one card (implies `guardrail`) |
//!
//! [`VisionToolset`] — `vision_detect`, `vision_segment`, `vision_read`, `vision_describe`,
//! `vision_crop`, `vision_redact`, `vision_annotate`, `vision_measure` — is always built: each tool runs on
//! the governed path, a remote call is held with the exact egress image as approval
//! evidence, and every call, release and edit writes its `vision.*` audit record
//! ([`audit`]).
//!
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
#[cfg(feature = "cloud")]
pub mod cloud;
#[cfg(feature = "conformance")]
pub mod conformance;
mod cx;
mod digest;
mod edit;
mod egress;
mod error;
#[cfg(feature = "eval")]
pub mod eval;
#[cfg(feature = "fetch")]
mod fetch;
mod geometry;
#[cfg(feature = "guardrail")]
mod guardrail;
mod manifest;
mod mask_decoder;
mod measure;
mod media;
#[cfg(feature = "memory")]
pub mod memory;
#[cfg(feature = "mistralrs")]
pub mod mistralrs;
#[cfg(feature = "nim")]
mod nim_cv;
#[cfg(feature = "ocr")]
mod ocr;
#[cfg(feature = "ocr-vl")]
mod ocr_vl;
#[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
pub mod onnx;
#[cfg(any(feature = "ocr", feature = "ocr-vl", feature = "nim"))]
mod reading;
mod redact;
mod runtime;
#[cfg(feature = "sam")]
pub mod sam;
pub mod screen;
mod sent;
#[cfg(feature = "stream")]
pub mod stream;
#[cfg(feature = "team")]
pub mod team;
mod toolset;
#[cfg(feature = "track")]
mod track;
mod tracker;
mod traits;
mod verify;
pub mod video;
mod watch;

pub use backend::{BackendDescriptor, BackendId, InputLimits, Locality, Oversize, PriceRef};
pub use budget::{ImageTokens, TokenEstimate};
#[cfg(feature = "cloud")]
pub use cloud::{LlmDescriber, RegionAdapter};
pub use cx::VisionCx;
pub use digest::Digest;
pub use edit::{DeterministicEditor, EditOp, Overlay, Rgba, apply_edit};
pub use egress::{ClipReleaseRequest, ReleasedClip};
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
    GateOutcome, ImagePrivacyGuardrail, MODEL_CONTEXT_CLASS, PERSONAL_FIELD_LABELS,
    PersonalTextRedactor,
};
pub use manifest::{
    AcceptanceStore, FileAcceptanceStore, InMemoryAcceptanceStore, InputSpec, IoSpec,
    LicenceAcceptance, LicenceClass, LicenceGate, LicenceInfo, LicencePolicy, ManifestSet,
    ModelFile, ModelManifest, ModelSource, RuntimeSpec, VerifiedFile, VerifiedModel, VisionTask,
};
pub use mask_decoder::{
    DEFAULT_MASK_DECODER_TIMEOUT_SECS, MaskDecoderChoice, SAM_MASK_CODEC_MANIFEST, SamSettings,
};
pub use measure::{
    Calibration, CalibrationSource, FiducialSpec, MEASUREMENT_VALIDITY, MeasuredGeometry,
    Measurement,
};
pub use media::{
    ArtifactLocator, DecodeLimits, DecodedImage, Frame, FrameMeta, FramePixels, FrameSourceId,
    ImageRef, MediaScope, MediaStore, Provenance, SourceImage, VideoRef, artifact_uri,
};
#[cfg(feature = "nim")]
pub use nim_cv::{
    DEFAULT_MAX_INLINE_BYTES, DEFAULT_MIN_LAYOUT_CONFIDENCE, DEFAULT_REQUESTS_PER_MINUTE,
    DEFAULT_TIMEOUT, GRAPHIC_ELEMENTS_MODEL, NIM_READER_ID, NimCvConfig, NimDocumentReader,
    NimOcrModel, PAGE_ELEMENTS_MODEL, TABLE_STRUCTURE_MODEL,
};
#[cfg(feature = "ocr")]
pub use ocr::{PpOcrReader, PpOcrV6Size};
#[cfg(feature = "ocr-vl")]
pub use ocr_vl::{PADDLEOCR_VL_MANIFEST, PP_DOCLAYOUT_MANIFEST, PaddleOcrVlReader, VlDevice};
pub use redact::{DEFAULT_PERSON_DETECTOR, PersonRedactor, REDACTION_MARGIN};
pub use runtime::{
    CallOptions, CallRecord, Edited, Escalation, Route, Trigger, VideoSource, VisionOutput,
    VisionRuntime, VisionRuntimeBuilder,
};
#[cfg(feature = "sam")]
pub use sam::{SamConfig, SamSegmenter, SamVideoTracker};
pub use sent::{SentImage, VisionInput};
pub use toolset::{
    ANSWER_ARTIFACT_PREFIX, EgressApproval, VISION_EGRESS_POLICY_REF, VISION_TOOL_NAMES,
    VisionTool, VisionToolKind, VisionToolset, VisionToolsetBuilder, answer_artifact_name,
};
#[cfg(feature = "track")]
pub use track::{ByteTrackConfig, ByteTracker};
pub use tracker::GreedyIouTracker;
pub use traits::{
    Block, BlockKind, DescribeRequest, DescribeSpec, Describer, Description, DetectQuery, Detector,
    DocumentPage, DocumentReader, Editor, Embedder, EmbeddingSpace, ImageRegion, Metric,
    ReadOptions, Sampling, SegmentPrompt, Segmenter, TableCell, Tracker, VideoAnalysis,
    VideoAnalyzer, VideoEvent, VideoFrame, VideoInput, VideoPlan, VideoRequest, VideoUsage, Word,
};
pub use verify::{
    ExpectedChange, LoopOutcome, Observation, Observer, RegionDiff, RegionExpectation,
    VERIFICATION_TOOL, VerificationHold, VerificationReport, VerifiedAction, VerifierLoop,
    check_change, diff_regions,
};
#[cfg(feature = "cloud")]
pub use video::FramesAnalyzer;
pub use video::{ClipInfo, MAX_CLIP_BYTES, SentClip, SourceClip, VideoCodec};
#[cfg(feature = "gemini-video")]
pub use video::{GEMINI_INLINE_LIMIT, GeminiVideoAnalyzer, MediaResolution};
pub use watch::{
    FrameRef, TrackedObservation, VISION_WATCH_FIRED, VISION_WATCH_REGISTERED, WatchCondition,
    WatchEvaluator, WatchEvent, WatchEventKind, WatchSink, Zone, ZoneAnchor,
    watch_registered_draft,
};
