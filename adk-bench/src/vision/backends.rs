//! The local backends this build can measure, by name, and the workload each runs.
//!
//! | Name | Feature | Task | Workload |
//! |------|---------|------|----------|
//! | `rf-detr-n`, `rf-detr-n-q4f16` | `vision-onnx` | detect | `generated-detect` |
//! | `efficientsam-ti`, `sam2.1-hiera-tiny-q4` | `vision-onnx` | segment | `shapes-segment` (box prompts) |
//! | `siglip2-base-patch16-256-quantized` | `vision-onnx` | embed | captioned fixtures |
//! | `pp-ocrv6-tiny`, `pp-ocrv6-small` | `vision-ocr` | read | `documents-read` |
//! | `paddleocr-vl-1.6-cpu`, `paddleocr-vl-1.6-metal` | `vision-ocr-vl`, `vision-ocr-vl-metal` | read | `documents-read` |
//!
//! Weights come through `adk-vision`'s fetcher (`HF_HUB_CACHE`, `HF_HUB_OFFLINE`), verified
//! against the built-in manifest; cold load includes that verification.

use std::sync::Arc;
use std::time::Duration;
#[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
use std::time::Instant;

use adk_vision::ManifestSet;
#[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ModelFetcher};

use super::{ModelRef, VisionBackend};
use crate::error::{BenchError, Result};

/// The scores reported for each task.
pub const DETECT_SCORES: &[&str] = &[
    "region_iou",
    "detection_precision",
    "detection_recall",
    "detection_ap",
    "detection_map",
    "count_accuracy",
];
/// Scores for promptable segmentation.
pub const SEGMENT_SCORES: &[&str] = &["mask_iou", "region_iou", "box_hit_rate", "count_accuracy"];
/// Scores for concept segmentation without masks.
pub const CONCEPT_SCORES: &[&str] = &["region_iou", "box_hit_rate", "detection_recall"];
/// Scores for document readers.
pub const READ_SCORES: &[&str] =
    &["ocr_char_accuracy", "ocr_word_accuracy", "field_exact_match", "field_normalized_match"];
/// Scores for describers asked for regions.
pub const LOCATE_SCORES: &[&str] =
    &["region_iou", "box_hit_rate", "detection_recall", "grounding_hit_rate"];

/// What a local backend is, before it is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalSpec {
    /// Its name.
    pub name: &'static str,
    /// The workload it runs: an eval set name, or `captioned-fixtures` for embedders.
    pub workload: &'static str,
    /// The scores reported.
    pub scores: &'static [&'static str],
}

/// Every local backend, and whether this build compiles it.
const LOCAL: &[(LocalSpec, bool)] = &[
    (
        LocalSpec { name: "rf-detr-n", workload: "generated-detect", scores: DETECT_SCORES },
        cfg!(feature = "vision-onnx"),
    ),
    (
        LocalSpec { name: "rf-detr-n-q4f16", workload: "generated-detect", scores: DETECT_SCORES },
        cfg!(feature = "vision-onnx"),
    ),
    (
        LocalSpec { name: "efficientsam-ti", workload: "shapes-segment", scores: SEGMENT_SCORES },
        cfg!(feature = "vision-onnx"),
    ),
    (
        LocalSpec {
            name: "sam2.1-hiera-tiny-q4",
            workload: "shapes-segment",
            scores: SEGMENT_SCORES,
        },
        cfg!(feature = "vision-onnx"),
    ),
    (
        LocalSpec {
            name: "siglip2-base-patch16-256-quantized",
            workload: "captioned-fixtures",
            scores: &[],
        },
        cfg!(feature = "vision-onnx"),
    ),
    (
        LocalSpec { name: "pp-ocrv6-tiny", workload: "documents-read", scores: READ_SCORES },
        cfg!(feature = "vision-ocr"),
    ),
    (
        LocalSpec { name: "pp-ocrv6-small", workload: "documents-read", scores: READ_SCORES },
        cfg!(feature = "vision-ocr"),
    ),
    (
        LocalSpec { name: "paddleocr-vl-1.6-cpu", workload: "documents-read", scores: READ_SCORES },
        cfg!(feature = "vision-ocr-vl"),
    ),
    (
        LocalSpec {
            name: "paddleocr-vl-1.6-metal",
            workload: "documents-read",
            scores: READ_SCORES,
        },
        cfg!(feature = "vision-ocr-vl-metal"),
    ),
];

/// The local backends compiled into this build.
pub fn local_specs() -> Vec<LocalSpec> {
    LOCAL.iter().filter(|(_, built)| *built).map(|(spec, _)| *spec).collect()
}

/// The built-in manifest entries a backend loads, as report rows.
///
/// # Errors
///
/// Returns [`BenchError::Vision`] when the built-in manifest does not parse.
pub fn model_refs(ids: &[&str]) -> Result<Vec<ModelRef>> {
    let manifests = ManifestSet::builtin().map_err(super::vision_error)?;
    Ok(ids
        .iter()
        .filter_map(|id| manifests.get(id))
        .map(|manifest| ModelRef {
            id: manifest.id.clone(),
            revision: serde_json::to_value(&manifest.source)
                .ok()
                .and_then(|source| source.get("revision")?.as_str().map(str::to_string)),
            licence: manifest.licence.id.clone(),
        })
        .collect())
}

/// An embedder backend: the runtime, its models and cold load.
pub struct LoadedEmbedder {
    /// The runtime with the embedder as its default.
    pub runtime: Arc<adk_vision::VisionRuntime>,
    /// Its models.
    pub models: Vec<ModelRef>,
    /// Time to verify and load.
    pub cold_load: Duration,
    /// The runtime label.
    pub runtime_label: String,
}

/// A loaded local backend.
pub enum Loaded {
    /// Answers eval cases.
    Agent(VisionBackend),
    /// Embeds images and texts.
    Embedder(LoadedEmbedder),
}

#[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
fn fetcher() -> ModelFetcher {
    ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
}

#[cfg(feature = "vision-onnx")]
async fn fetch(id: &str) -> Result<adk_vision::VerifiedModel> {
    let manifests = ManifestSet::builtin().map_err(super::vision_error)?;
    let manifest =
        manifests.get(id).ok_or_else(|| BenchError::Vision(format!("{id} is not built in")))?;
    fetcher().fetch(manifest).await.map_err(super::vision_error)
}

/// The onnxruntime release this build links, such as `1.28.0`, from its build info.
#[cfg(feature = "vision-onnx")]
pub fn onnxruntime_version() -> Option<String> {
    let info = ort::info();
    let release = info.split("git-branch=rel-").nth(1)?;
    Some(release.split([',', ' ']).next()?.to_string())
}

/// The ONNX execution provider from `ADK_VISION_ONNX_EP` (`cpu`, `coreml`, `cuda`).
#[cfg(feature = "vision-onnx")]
pub fn onnx_options() -> adk_vision::onnx::OnnxOptions {
    use adk_vision::onnx::{OnnxExecutionProvider, OnnxOptions};
    let provider = match std::env::var("ADK_VISION_ONNX_EP").as_deref() {
        Ok("coreml") => OnnxExecutionProvider::CoreMl,
        Ok("cuda") => OnnxExecutionProvider::Cuda,
        _ => OnnxExecutionProvider::Cpu,
    };
    OnnxOptions::default().with_execution_provider(provider)
}

/// Loads the local backend `name`, fetching and verifying its weights.
///
/// # Errors
///
/// Returns [`BenchError::Vision`] for a name this build does not include, or when the
/// weights cannot be fetched or loaded.
pub async fn load_local(name: &str) -> Result<Loaded> {
    #[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
    use adk_vision::VisionRuntime;
    #[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
    use adk_vision::eval::{EvalTask, VisionEvalAgent};

    #[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
    let started = Instant::now();
    #[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
    let agent = |id: &str, runtime: VisionRuntime, task: EvalTask, label: &str| {
        let runtime = Arc::new(runtime);
        VisionBackend::new(id, label, super::RunMode::Local, |cx| {
            VisionEvalAgent::new(runtime, task).with_name(id).with_context(cx)
        })
    };
    #[cfg(any(feature = "vision-onnx", feature = "vision-ocr", feature = "vision-ocr-vl"))]
    let built = |e: adk_vision::VisionError| BenchError::Vision(format!("{name}: {e}"));
    match name {
        #[cfg(feature = "vision-onnx")]
        "rf-detr-n" | "rf-detr-n-q4f16" => {
            use adk_vision::onnx::RfDetr;
            // The 4-bit export scores the same objects lower, so it reports from 0.3.
            let threshold = if name == "rf-detr-n" { 0.5 } else { 0.3 };
            let model = fetch(name).await?;
            let detector = RfDetr::new(&model, &onnx_options()).map_err(built)?;
            let runtime = VisionRuntime::builder()
                .detector(Arc::new(detector.with_threshold(threshold)))
                .build()
                .map_err(built)?;
            let label = format!("ort, {}", onnx_options().execution_provider);
            let task = EvalTask::Detect { concepts: false };
            Ok(Loaded::Agent(
                agent(name, runtime, task, &label)
                    .with_models(model_refs(&[name])?)
                    .with_cold_load(started.elapsed()),
            ))
        }
        #[cfg(feature = "vision-onnx")]
        "efficientsam-ti" | "sam2.1-hiera-tiny-q4" => {
            use adk_vision::onnx::{EfficientSam, Sam2};
            let model = fetch(name).await?;
            let segmenter: Arc<dyn adk_vision::Segmenter> = if name == "efficientsam-ti" {
                Arc::new(EfficientSam::new(&model, &onnx_options()).map_err(built)?)
            } else {
                Arc::new(Sam2::new(&model, &onnx_options()).map_err(built)?)
            };
            let runtime = VisionRuntime::builder().segmenter(segmenter).build().map_err(built)?;
            let label = format!("ort, {}", onnx_options().execution_provider);
            Ok(Loaded::Agent(
                agent(name, runtime, EvalTask::Segment, &label)
                    .with_models(model_refs(&[name])?)
                    .with_cold_load(started.elapsed()),
            ))
        }
        #[cfg(feature = "vision-onnx")]
        "siglip2-base-patch16-256-quantized" => {
            use adk_vision::onnx::SigLip2;
            let model = fetch(name).await?;
            let embedder = SigLip2::new(&model, &onnx_options()).map_err(built)?;
            let runtime =
                VisionRuntime::builder().embedder(Arc::new(embedder)).build().map_err(built)?;
            Ok(Loaded::Embedder(LoadedEmbedder {
                runtime: Arc::new(runtime),
                models: model_refs(&[name])?,
                cold_load: started.elapsed(),
                runtime_label: format!("ort, {}", onnx_options().execution_provider),
            }))
        }
        #[cfg(feature = "vision-ocr")]
        "pp-ocrv6-tiny" | "pp-ocrv6-small" => {
            use adk_vision::{PpOcrReader, PpOcrV6Size};
            let size = if name == "pp-ocrv6-tiny" { PpOcrV6Size::Tiny } else { PpOcrV6Size::Small };
            let reader = PpOcrReader::fetch(&fetcher(), size).await.map_err(built)?;
            let runtime =
                VisionRuntime::builder().reader(Arc::new(reader)).build().map_err(built)?;
            let task = EvalTask::Read { markdown: false, fields: true };
            let models = model_refs(&[&size.detection_manifest(), &size.recognition_manifest()])?;
            Ok(Loaded::Agent(
                agent(name, runtime, task, "oar-ocr on ort, CPU")
                    .with_models(models)
                    .with_cold_load(started.elapsed()),
            ))
        }
        #[cfg(feature = "vision-ocr-vl")]
        "paddleocr-vl-1.6-cpu" | "paddleocr-vl-1.6-metal" => {
            use adk_vision::{
                PADDLEOCR_VL_MANIFEST, PP_DOCLAYOUT_MANIFEST, PaddleOcrVlReader, VlDevice,
            };
            let (device, label) = if name.ends_with("metal") {
                (VlDevice::Metal, "oar-ocr-vl on Candle, Metal")
            } else {
                (VlDevice::Cpu, "oar-ocr-vl on Candle, CPU")
            };
            let reader = PaddleOcrVlReader::fetch(&fetcher(), device).await.map_err(built)?;
            let runtime =
                VisionRuntime::builder().reader(Arc::new(reader)).build().map_err(built)?;
            let task = EvalTask::Read { markdown: false, fields: true };
            Ok(Loaded::Agent(
                agent(name, runtime, task, label)
                    .with_models(model_refs(&[PADDLEOCR_VL_MANIFEST, PP_DOCLAYOUT_MANIFEST])?)
                    .with_cold_load(started.elapsed()),
            ))
        }
        other => Err(BenchError::Vision(format!(
            "{other} is not a local backend in this build; it has: {}",
            local_specs().iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_refs_carry_revision_and_licence() {
        let refs = model_refs(&["efficientsam-ti", "not-a-model"]).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].licence, "Apache-2.0");
        assert_eq!(refs[0].revision.as_deref().map(str::len), Some(40));
    }

    #[tokio::test]
    async fn unknown_backends_name_the_ones_built_in() {
        let Err(err) = load_local("yolo26").await else { panic!("yolo26 must not load") };
        assert!(err.to_string().contains("not a local backend"), "{err}");
    }
}
