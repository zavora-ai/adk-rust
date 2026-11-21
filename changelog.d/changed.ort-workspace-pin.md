- **One `ort` pin at 2.0.0-rc.15, ONNX Runtime 1.31** (`adk-audio`, `adk-rag`, `adk-vision`):
  the workspace pins `ort` once, at the version `oar-ocr` 0.11 pins, so every crate that runs
  ONNX models resolves in one build. `ort` still downloads the onnxruntime binary at build time.
  `OnnxExecutionProvider::Cuda` and `CoreMl` now register through the new `onnx-cuda` and
  `onnx-coreml` features of `adk-audio`; without its feature a provider logs a warning and the
  session runs on the CPU, as it did before.
- **One ONNX Runtime environment per process** (`adk-core`, feature `ort`): `ort` allows one,
  so `adk-audio`, `adk-rag` and `adk-vision` take it from `adk_core::onnx::ort_environment`, and
  `adk-vision` hands it to `oar-ocr`. An application installs its own with
  `adk_core::onnx::set_ort_environment` before the first model loads.
- **ONNX models run on the CPU unless an accelerator is chosen** (`adk-audio`, `adk-rag`,
  `adk-vision`): no crate on the pin turns on an `ort` API level of 22 or more, from which `ort`
  lets onnxruntime pick each session's device, CoreML on Apple silicon. `adk-audio` and
  `adk-rag` sessions on Apple silicon no longer move to CoreML on their own; PP-OCRv6 keeps its
  CPU speed and memory under onnxruntime 1.31.
