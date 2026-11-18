//! [`OnnxOptions`], [`OnnxExecutionProvider`], and the session wrapper every backend runs on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ort::ep::ExecutionProviderDispatch;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::{DynValue, Tensor};
use serde::{Deserialize, Serialize};

use crate::error::{Result, VisionError};
use crate::manifest::{ModelManifest, RuntimeSpec, VerifiedFile, VerifiedModel, VisionTask};

/// The hardware an ONNX session runs on, chosen at run time.
///
/// The CPU is the default and always available. CoreML (Apple Neural Engine and GPU)
/// needs feature `onnx-coreml`, CUDA needs `onnx-cuda`. A provider this build does not
/// compile logs a warning and runs on the CPU; `ort` itself falls back to the CPU, with a
/// warning, when a compiled provider cannot start on this machine.
///
/// # Example
///
/// ```
/// use adk_vision::onnx::OnnxExecutionProvider;
///
/// assert_eq!(OnnxExecutionProvider::default(), OnnxExecutionProvider::Cpu);
/// assert_eq!(OnnxExecutionProvider::CoreMl.to_string(), "CoreML");
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OnnxExecutionProvider {
    /// The CPU.
    #[default]
    Cpu,
    /// CoreML on Apple silicon (feature `onnx-coreml`).
    CoreMl,
    /// NVIDIA CUDA (feature `onnx-cuda`).
    Cuda,
}

impl OnnxExecutionProvider {
    /// The provider to register on a session builder, or `None` to run on the CPU.
    fn dispatch(self) -> Option<ExecutionProviderDispatch> {
        let provider = match self {
            Self::Cpu => return None,
            #[cfg(feature = "onnx-coreml")]
            Self::CoreMl => Some(ort::ep::CoreML::default().build()),
            #[cfg(not(feature = "onnx-coreml"))]
            Self::CoreMl => None,
            #[cfg(feature = "onnx-cuda")]
            Self::Cuda => Some(ort::ep::CUDA::default().build()),
            #[cfg(not(feature = "onnx-cuda"))]
            Self::Cuda => None,
        };
        if provider.is_none() {
            tracing::warn!(
                onnx.execution_provider = %self,
                "execution provider not compiled into this build, running on the cpu"
            );
        }
        provider
    }
}

impl std::fmt::Display for OnnxExecutionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cpu => "CPU",
            Self::CoreMl => "CoreML",
            Self::Cuda => "CUDA",
        })
    }
}

/// How an ONNX backend's sessions run.
///
/// # Example
///
/// ```
/// use adk_vision::onnx::{OnnxExecutionProvider, OnnxOptions};
///
/// let options = OnnxOptions::default()
///     .with_execution_provider(OnnxExecutionProvider::CoreMl)
///     .with_intra_threads(4);
/// assert_eq!(options.intra_threads, Some(4));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct OnnxOptions {
    /// The execution provider; the CPU by default.
    #[serde(default)]
    pub execution_provider: OnnxExecutionProvider,
    /// Threads one operator may use; onnxruntime's choice when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intra_threads: Option<usize>,
}

impl OnnxOptions {
    /// Runs on `provider`.
    pub fn with_execution_provider(mut self, provider: OnnxExecutionProvider) -> Self {
        self.execution_provider = provider;
        self
    }

    /// Lets one operator use `threads` threads.
    pub fn with_intra_threads(mut self, threads: usize) -> Self {
        self.intra_threads = Some(threads);
        self
    }
}

/// One input tensor, owned until the blocking run turns it into an `ort` value.
pub(crate) enum Input {
    F32(Vec<i64>, Vec<f32>),
    I64(Vec<i64>, Vec<i64>),
}

/// One output tensor copied out of the session: its shape and values.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Output {
    pub shape: Vec<i64>,
    pub data: Vec<f32>,
}

impl Output {
    /// The shape as `usize` dimensions.
    pub fn dims(&self) -> Vec<usize> {
        self.shape.iter().map(|&d| usize::try_from(d).unwrap_or(0)).collect()
    }
}

/// An ONNX session behind a mutex, run on a blocking thread.
#[derive(Clone)]
pub(crate) struct OrtModel {
    backend: Arc<str>,
    session: Arc<Mutex<Session>>,
    inputs: Arc<[String]>,
    outputs: Arc<[(String, Vec<i64>)]>,
}

impl std::fmt::Debug for OrtModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrtModel")
            .field("backend", &self.backend)
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .finish_non_exhaustive()
    }
}

impl OrtModel {
    /// Loads a verified file. External data (`.onnx_data`) is read from beside it.
    pub fn load(backend: &str, file: &VerifiedFile, options: &OnnxOptions) -> Result<Self> {
        let failed = |message: String| VisionError::Backend { backend: backend.into(), message };
        let mut builder = Session::builder()
            .and_then(|b| {
                b.with_optimization_level(GraphOptimizationLevel::Level3).map_err(Into::into)
            })
            .map_err(|e| failed(format!("the session builder failed to start: {e}")))?;
        if let Some(threads) = options.intra_threads {
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| failed(format!("could not set {threads} intra-op threads: {e}")))?;
        }
        if let Some(provider) = options.execution_provider.dispatch() {
            builder = builder.with_execution_providers([provider]).map_err(|e| {
                failed(format!(
                    "could not register the {} execution provider: {e}",
                    options.execution_provider
                ))
            })?;
        }
        let session = builder
            .commit_from_file(file.path())
            .map_err(|e| failed(format!("could not load {}: {e}", file.path().display())))?;
        let inputs = session.inputs().iter().map(|i| i.name().to_string()).collect();
        let outputs = session
            .outputs()
            .iter()
            .map(|o| {
                let shape = match o.dtype() {
                    ort::value::ValueType::Tensor { shape, .. } => shape.to_vec(),
                    _ => Vec::new(),
                };
                (o.name().to_string(), shape)
            })
            .collect();
        tracing::info!(
            vision.backend = backend,
            model.file = %file.path().display(),
            onnx.execution_provider = %options.execution_provider,
            "onnx session loaded"
        );
        Ok(Self {
            backend: backend.into(),
            session: Arc::new(Mutex::new(session)),
            inputs,
            outputs,
        })
    }

    /// The graph's output names and declared shapes (`-1` for a dynamic dimension).
    pub fn outputs(&self) -> &[(String, Vec<i64>)] {
        &self.outputs
    }

    /// The first input whose name is one of `names`, else the input at `fallback`.
    pub fn input_named(&self, names: &[&str], fallback: usize) -> Result<String> {
        self.inputs
            .iter()
            .find(|i| names.contains(&i.as_str()))
            .or_else(|| self.inputs.get(fallback))
            .cloned()
            .ok_or_else(|| self.error(format!("the graph has no input named any of {names:?}")))
    }

    /// Runs the graph on a blocking thread and copies out the named outputs as `f32`.
    pub async fn run(
        &self,
        inputs: Vec<(String, Input)>,
        wanted: &[&str],
    ) -> Result<HashMap<String, Output>> {
        let model = self.clone();
        let wanted: Vec<String> = wanted.iter().map(|w| (*w).to_string()).collect();
        tokio::task::spawn_blocking(move || model.run_blocking(inputs, &wanted))
            .await
            .map_err(|e| self.error(format!("the inference task failed: {e}")))?
    }

    fn run_blocking(
        &self,
        inputs: Vec<(String, Input)>,
        wanted: &[String],
    ) -> Result<HashMap<String, Output>> {
        let mut values: Vec<(String, DynValue)> = Vec::with_capacity(inputs.len());
        for (name, input) in inputs {
            let value = match input {
                Input::F32(shape, data) => Tensor::from_array((shape, data)).map(Tensor::into_dyn),
                Input::I64(shape, data) => Tensor::from_array((shape, data)).map(Tensor::into_dyn),
            }
            .map_err(|e| self.error(format!("input `{name}` is not a valid tensor: {e}")))?;
            values.push((name, value));
        }
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let outputs =
            session.run(values).map_err(|e| self.error(format!("inference failed: {e}")))?;
        let mut copied = HashMap::with_capacity(wanted.len());
        for name in wanted {
            let value = outputs
                .get(name.as_str())
                .ok_or_else(|| self.error(format!("the graph produced no output `{name}`")))?;
            let (shape, data) = value
                .try_extract_tensor::<f32>()
                .map_err(|e| self.error(format!("output `{name}` is not an f32 tensor: {e}")))?;
            copied.insert(name.clone(), Output { shape: shape.to_vec(), data: data.to_vec() });
        }
        Ok(copied)
    }

    /// A backend error naming this model's backend.
    pub fn error(&self, message: String) -> VisionError {
        VisionError::Backend { backend: self.backend.to_string(), message }
    }
}

/// Checks that `manifest` is an `ort` model for `task`.
pub(crate) fn check_manifest(manifest: &ModelManifest, task: VisionTask) -> Result<()> {
    if manifest.task != task {
        return Err(manifest_error(
            manifest,
            format!(
                "the backend runs {task:?} models; the manifest is a {:?} model",
                manifest.task
            ),
        ));
    }
    if !matches!(manifest.runtime, RuntimeSpec::Ort { .. }) {
        return Err(manifest_error(
            manifest,
            format!("the backend runs on `ort`; the manifest names {:?}", manifest.runtime),
        ));
    }
    Ok(())
}

/// The verified `.onnx` file of `model` whose name contains `role` and not `unless`, or its
/// only `.onnx` file when `role` is empty.
pub(crate) fn onnx_file<'m>(
    model: &'m VerifiedModel,
    role: &str,
    unless: &str,
) -> Result<&'m VerifiedFile> {
    let manifest = model.manifest();
    let path = onnx_path(manifest, role, unless)?;
    model.file(path).ok_or_else(|| manifest_error(manifest, format!("`{path}` was not verified")))
}

/// The manifest path [`onnx_file`] picks.
fn onnx_path<'a>(manifest: &'a ModelManifest, role: &str, unless: &str) -> Result<&'a str> {
    let onnx: Vec<&str> =
        manifest.files.iter().map(|f| f.path.as_str()).filter(|p| p.ends_with(".onnx")).collect();
    let named: Vec<&str> = onnx
        .iter()
        .copied()
        .filter(|p| {
            let name = file_name(p);
            name.contains(role) && (unless.is_empty() || !name.contains(unless))
        })
        .collect();
    match named.as_slice() {
        [path] => Ok(path),
        _ => {
            let wanted = if role.is_empty() {
                "exactly one `.onnx` file".to_string()
            } else {
                format!("exactly one `.onnx` file whose name contains `{role}`")
            };
            Err(manifest_error(
                manifest,
                format!("the backend needs {wanted}; the manifest pins {onnx:?}"),
            ))
        }
    }
}

/// The verified file of `model` named exactly `name` (its last path segment).
pub(crate) fn named_file<'m>(model: &'m VerifiedModel, name: &str) -> Result<&'m VerifiedFile> {
    let manifest = model.manifest();
    let path =
        manifest.files.iter().map(|f| f.path.as_str()).find(|p| file_name(p) == name).ok_or_else(
            || manifest_error(manifest, format!("the backend needs a `{name}` file")),
        )?;
    model.file(path).ok_or_else(|| manifest_error(manifest, format!("`{path}` was not verified")))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A manifest error for `manifest`.
pub(crate) fn manifest_error(manifest: &ModelManifest, reason: String) -> VisionError {
    VisionError::Manifest { id: manifest.id.clone(), reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{InMemoryAcceptanceStore, LicenceGate, LicencePolicy, ManifestSet};

    #[test]
    fn roles_pick_the_encoder_and_the_decoder_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("onnx")).unwrap();
        let paths = [
            "onnx/vision_encoder_q4.onnx",
            "onnx/vision_encoder_q4.onnx_data",
            "onnx/prompt_encoder_mask_decoder_q4.onnx",
        ];
        let mut files = Vec::new();
        for path in paths {
            std::fs::write(dir.path().join(path), path.as_bytes()).unwrap();
            files.push(serde_json::json!({
                "path": path,
                "sha256": crate::digest::Digest::of(path.as_bytes()),
                "size": path.len(),
            }));
        }
        let manifest = ModelManifest::from_json(
            &serde_json::json!({
                "id": "toy-sam", "task": "segment",
                "source": {"kind": "local", "dir": dir.path()},
                "files": files,
                "licence": {"id": "Apache-2.0", "url": "https://www.apache.org/licenses/LICENSE-2.0", "class": "permissive"},
                "runtime": {"kind": "ort", "opset": 18}
            })
            .to_string(),
        )
        .unwrap();
        let (policy, store) = (LicencePolicy::new(), InMemoryAcceptanceStore::new());
        let model =
            VerifiedModel::check_dir(&manifest, dir.path(), &LicenceGate::new(&policy, &store))
                .unwrap();
        let path = |role, unless| onnx_file(&model, role, unless).map(|f| f.path().to_path_buf());
        assert_eq!(path("encoder", "decoder").unwrap(), dir.path().join(paths[0]));
        assert_eq!(path("decoder", "").unwrap(), dir.path().join(paths[2]));
        // Both files contain `encoder`, and two `.onnx` files are not one.
        assert!(matches!(path("encoder", ""), Err(VisionError::Manifest { .. })));
        assert!(matches!(path("", ""), Err(VisionError::Manifest { .. })));
        assert!(check_manifest(&manifest, VisionTask::Detect).is_err());
        assert!(check_manifest(&manifest, VisionTask::Segment).is_ok());
    }

    #[test]
    fn every_built_in_onnx_backend_model_has_what_its_backend_reads() {
        let builtin = ManifestSet::builtin().unwrap();
        let families = ["rf-detr-", "efficientsam-", "sam2.1-", "siglip2-"];
        let ort: Vec<_> =
            builtin.iter().filter(|m| families.iter().any(|f| m.id.starts_with(f))).collect();
        assert_eq!(ort.len(), 10, "the built-in RF-DETR, SAM-family and SigLIP 2 records");
        for m in ort {
            assert!(m.io.input.is_some(), "`{}` names no input", m.id);
            match m.task {
                VisionTask::Detect => {
                    onnx_path(m, "", "").unwrap();
                    assert_eq!(m.io.labels.len(), 91, "`{}`", m.id);
                    assert_eq!(m.io.labels.iter().filter(|l| !l.is_empty()).count(), 80);
                    assert_eq!(
                        (m.io.labels[1].as_str(), m.io.labels[90].as_str()),
                        ("person", "toothbrush")
                    );
                }
                VisionTask::Segment => {
                    onnx_path(m, "encoder", "decoder").unwrap();
                    onnx_path(m, "decoder", "").unwrap();
                }
                VisionTask::Embed => {
                    onnx_path(m, "vision", "").unwrap();
                    onnx_path(m, "text", "").unwrap();
                    assert!(m.files.iter().any(|f| f.path == "tokenizer.json"), "`{}`", m.id);
                }
                other => panic!("`{}` is a built-in {other:?} model with no ort backend", m.id),
            }
        }
    }
}
