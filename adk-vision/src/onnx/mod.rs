//! Local backends on ONNX Runtime through `ort` (spec 3.7 §7.6), features `onnx` and
//! `onnx-dynamic`.
//!
//! | Backend | Trait | Models | Answers in |
//! |---------|-------|--------|------------|
//! | [`RfDetr`] | [`Detector`](crate::Detector) | RF-DETR N, S and M; an RF-DETR-Seg export adds instance masks | `ModelInput` with the manifest's letterbox |
//! | [`EfficientSam`] | [`Segmenter`](crate::Segmenter) | EfficientSAM-Ti (split encoder and decoder) | `SentPixels` |
//! | [`Sam2`] | [`Segmenter`](crate::Segmenter) | SAM 2.1 Tiny (split encoder and decoder) | `ModelInput` 1024×1024, stretched |
//! | [`SigLip2`] | [`Embedder`](crate::Embedder) | SigLIP 2 base (so400m opt-in) | — |
//! | `Sam3`, `Sam3Tracker` (feature `sam3-local`) | [`Segmenter`](crate::Segmenter), [`Detector`](crate::Detector) | SAM 3 from weights you supply under Meta's SAM License, after you accept it | `ModelInput` 1008×1008, stretched |
//!
//! Every backend is built from a [`VerifiedModel`](crate::VerifiedModel), so no file loads
//! unless its SHA-256 matched its manifest (V-I10), and reads its input size, normalisation
//! and labels from the manifest's [`IoSpec`](crate::IoSpec): a user's own export, such as an
//! RF-DETR fine-tune, runs through the same backend with its own manifest. Each backend is
//! on-device ([`Locality::InProcess`](crate::Locality::InProcess)), so the runtime records
//! its units at zero cost and it never touches the egress gate.
//!
//! Inference runs on a blocking thread, so a call does not stall the async runtime and the
//! runtime's cancellation ends the call at once. The SAM-family encoders run once per
//! prepared image and their decoders once per prompt group.
//!
//! [`OnnxOptions`] chooses the execution provider at run time: the CPU by default, CoreML
//! on Apple (feature `onnx-coreml`) and CUDA (feature `onnx-cuda`). A provider this build
//! does not compile, or one that cannot start on this machine, falls back to the CPU with a
//! warning.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use adk_vision::onnx::{OnnxOptions, RfDetr};
//! use adk_vision::{
//!     DecodeLimits, DetectQuery, FetchConfig, InMemoryAcceptanceStore, ManifestSet,
//!     ModelFetcher, SourceImage, VisionCx, VisionRuntime,
//! };
//!
//! # async fn demo() -> adk_vision::Result<()> {
//! let manifests = ManifestSet::builtin()?;
//! let fetcher =
//!     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
//! let model = fetcher.fetch(manifests.get("rf-detr-n").expect("built in")).await?;
//! let detector = RfDetr::new(&model, &OnnxOptions::default())?;
//!
//! let runtime = VisionRuntime::builder().detector(Arc::new(detector)).build()?;
//! let bytes = std::fs::read("street.jpg").expect("a photo");
//! let photo = SourceImage::decode(&bytes, &DecodeLimits::default())?;
//! let found = runtime
//!     .detect(&VisionCx::new(), &photo, &DetectQuery::Classes(vec!["person".into()]))
//!     .await?;
//! for region in &found.value {
//!     println!("{:?} {:?} {:?}", region.label, region.score, region.shape.bbox());
//! }
//! # Ok(())
//! # }
//! ```

mod efficient_sam;
mod embedding_cache;
mod preprocess;
mod rf_detr;
mod sam;
mod sam2;
#[cfg(feature = "sam3-local")]
mod sam3;
mod session;
mod siglip;

pub use efficient_sam::EfficientSam;
pub use rf_detr::RfDetr;
pub use sam2::Sam2;
#[cfg(feature = "sam3-local")]
pub use sam3::{SAM_LICENCE_ID, Sam3, Sam3Tracker};
pub use session::{OnnxExecutionProvider, OnnxOptions};
pub use siglip::SigLip2;
