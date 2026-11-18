//! ONNX Runtime execution provider selection and auto-detection.

use ort::ep::ExecutionProviderDispatch;

/// Hardware execution provider for ONNX Runtime inference.
///
/// Auto-detection priority: CUDA > CoreML > CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnnxExecutionProvider {
    /// NVIDIA CUDA (Linux, Windows).
    Cuda,
    /// DirectML (Windows — AMD, Intel, NVIDIA GPUs).
    DirectMl,
    /// CoreML (macOS — Apple Neural Engine + GPU).
    CoreMl,
    /// CPU fallback (always available).
    Cpu,
}

impl OnnxExecutionProvider {
    /// Auto-detect the best available execution provider.
    ///
    /// Priority: CUDA > DirectML > CoreML > CPU.
    pub fn auto_detect() -> Self {
        if cfg!(target_os = "macos") {
            // On macOS, prefer CoreML
            OnnxExecutionProvider::CoreMl
        } else {
            // Default to CPU; CUDA/DirectML detection requires runtime checks
            // that depend on installed drivers.
            OnnxExecutionProvider::Cpu
        }
    }

    /// The `ort` provider to register on a session builder, or `None` to run on the CPU.
    ///
    /// A provider this build does not compile — CUDA without the `onnx-cuda` feature, CoreML
    /// without `onnx-coreml`, and DirectML — logs a warning and returns `None`. `ort` itself
    /// falls back to the CPU, with a warning, when a compiled provider cannot start on this
    /// machine.
    pub(crate) fn dispatch(self) -> Option<ExecutionProviderDispatch> {
        let provider = match self {
            #[cfg(feature = "onnx-cuda")]
            Self::Cuda => Some(ort::ep::CUDA::default().build()),
            #[cfg(feature = "onnx-coreml")]
            Self::CoreMl => Some(ort::ep::CoreML::default().build()),
            #[cfg(not(feature = "onnx-cuda"))]
            Self::Cuda => None,
            #[cfg(not(feature = "onnx-coreml"))]
            Self::CoreMl => None,
            Self::DirectMl => None,
            Self::Cpu => return None,
        };
        if provider.is_none() {
            tracing::warn!(
                onnx.execution_provider = %self,
                "execution provider not available in this build, falling back to cpu"
            );
        }
        provider
    }
}

impl std::fmt::Display for OnnxExecutionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cuda => write!(f, "CUDA"),
            Self::DirectMl => write!(f, "DirectML"),
            Self::CoreMl => write!(f, "CoreML"),
            Self::Cpu => write!(f, "CPU"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_registers_no_provider() {
        assert!(OnnxExecutionProvider::Cpu.dispatch().is_none());
    }

    #[test]
    fn a_provider_registers_only_when_its_feature_is_compiled() {
        assert_eq!(OnnxExecutionProvider::Cuda.dispatch().is_some(), cfg!(feature = "onnx-cuda"));
        assert_eq!(
            OnnxExecutionProvider::CoreMl.dispatch().is_some(),
            cfg!(feature = "onnx-coreml")
        );
        assert!(OnnxExecutionProvider::DirectMl.dispatch().is_none());
    }
}
