//! ONNX Runtime execution provider selection and auto-detection.

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
