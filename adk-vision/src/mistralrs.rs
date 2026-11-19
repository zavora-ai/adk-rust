//! A describer on this machine over mistral.rs (spec 3.7 §7.6), feature `mistralrs`.
//!
//! [`LlmDescriber::mistralrs`] loads a vision-language model verified against its manifest
//! into this process through `adk-mistralrs`. The model answers `Llm::data_locality()`
//! `InProcess`, so the describer takes [`SentImage`]s, never touches the egress gate, and the
//! privacy guardrail does not guard it; its calls are metered at zero cost.
//!
//! | Manifest | Model | Regions | Weights |
//! |----------|-------|---------|---------|
//! | [`QWEN3_VL_2B`] | `Qwen/Qwen3-VL-2B-Instruct` | `bbox_2d` on 0–1000, x first: the model's own frame ([`Normalized1000::qwen3_vl`]) | 4.26 GB safetensors, Apache-2.0 |
//!
//! mistral.rs 0.8 loads Qwen3-VL from safetensors only; there is no GGUF path for its
//! vision tower. A manifest for another mistral.rs vision architecture loads too and answers
//! in text and values; its regions are refused until it has a region adapter.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use adk_mistralrs::MistralRsConfig;
//! use adk_vision::mistralrs::QWEN3_VL_2B;
//! use adk_vision::{
//!     DecodeLimits, DescribeSpec, FetchConfig, InMemoryAcceptanceStore, LlmDescriber,
//!     ManifestSet, ModelFetcher, SourceImage, VisionCx, VisionRuntime,
//! };
//!
//! # async fn run() -> adk_vision::Result<()> {
//! let fetcher =
//!     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
//! let manifest = ManifestSet::builtin()?.get(QWEN3_VL_2B).cloned().expect("built in");
//! let model = fetcher.fetch(&manifest).await?;
//! let describer = LlmDescriber::mistralrs(&model, MistralRsConfig::default()).await?;
//!
//! let runtime = VisionRuntime::builder().describer(Arc::new(describer)).build()?;
//! let photo = SourceImage::decode(&std::fs::read("street.jpg").unwrap(), &DecodeLimits::default())?;
//! let spec = DescribeSpec::new("Where is every person?").with_regions();
//! let answer = runtime.describe(&VisionCx::new(), &[&photo], &spec).await?;
//! // Every region is in source pixels of the photo.
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use adk_mistralrs::{MistralRsConfig, MistralRsVisionModel, ModelArchitecture, ModelSource};

use crate::backend::InputLimits;
use crate::cloud::{LlmDescriber, Normalized1000};
use crate::error::{Result, VisionError};
use crate::manifest::{RuntimeSpec, VerifiedModel, VisionTask};
use crate::sent::SentImage;

/// The built-in manifest of Qwen3-VL 2B Instruct, the smallest Qwen3-VL.
pub const QWEN3_VL_2B: &str = "qwen3-vl-2b-instruct";

/// The most pixels a mistral.rs describer is handed: the runtime downscales a larger image
/// and records the resize, so a call on the CPU stays near 1,000 image tokens. Raise it with
/// `LlmDescriber::with_limits` for small print.
pub const MAX_PIXELS: u64 = 1_048_576;

impl LlmDescriber<SentImage> {
    /// Loads `model`, a mistral.rs vision-language model verified against its manifest, into
    /// this process with `config`'s device, quantisation and context settings; the model
    /// source is the verified directory.
    ///
    /// The describer is named by the manifest id. A `qwen3vl` architecture answers regions
    /// with [`Normalized1000::qwen3_vl`]; any other answers in text and values only.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when the manifest is not a `describe` model on the
    /// `mistralRs` runtime, its files are not in one directory, or that directory holds
    /// weights the manifest does not pin; [`VisionError::Backend`] when mistral.rs cannot
    /// load it.
    pub async fn mistralrs(model: &VerifiedModel, config: MistralRsConfig) -> Result<Self> {
        let manifest = model.manifest();
        let invalid = |reason: &str| VisionError::Manifest {
            id: manifest.id.clone(),
            reason: reason.to_string(),
        };
        if manifest.task != VisionTask::Describe {
            return Err(invalid("a mistral.rs describer loads a `describe` model"));
        }
        let RuntimeSpec::MistralRs { architecture } = &manifest.runtime else {
            return Err(invalid("the manifest's runtime is not `mistralRs`"));
        };
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        let dir = model.dir_of(&paths)?;
        let config = MistralRsConfig {
            model_source: ModelSource::Local(dir),
            architecture: ModelArchitecture::Vision,
            ..config
        };
        let llm = MistralRsVisionModel::new(config).await.map_err(|e| VisionError::Backend {
            backend: manifest.id.clone(),
            message: format!("mistral.rs could not load the model: {e}"),
        })?;
        tracing::info!(vision.backend = %manifest.id, model.architecture = %architecture, "mistral.rs describer loaded");
        let describer = LlmDescriber::local(Arc::new(llm))?.with_id(manifest.id.clone());
        let describer = match architecture.as_str() {
            "qwen3vl" | "qwen3_vl" => {
                describer.with_adapter(Arc::new(Normalized1000::qwen3_vl(&manifest.id)))
            }
            _ => describer.without_regions(),
        };
        Ok(describer
            .with_manifest(&manifest.id)
            .with_limits(InputLimits::unlimited().with_max_pixels(MAX_PIXELS)))
    }
}
