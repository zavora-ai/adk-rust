- **One `ort` pin at 2.0.0-rc.13, ONNX Runtime 1.28** (`adk-audio`, `adk-rag`): the workspace
  pins `ort` once, at the version `oar-ocr` 0.10 pins, so every crate that runs ONNX models
  resolves in one build. `ort` still downloads the onnxruntime binary at build time.
  `OnnxExecutionProvider::Cuda` and `CoreMl` now register through the new `onnx-cuda` and
  `onnx-coreml` features of `adk-audio`; without its feature a provider logs a warning and the
  session runs on the CPU, as it did before.
