- **Local ONNX backends for `adk-vision`** (`adk-vision`, features `onnx`, `onnx-dynamic`,
  `onnx-coreml`, `onnx-cuda`; spec 3.7 §7.6): `RfDetr` detects COCO classes, or any
  fine-tune's own labels, and returns instance masks from an RF-DETR-Seg export;
  `EfficientSam` and `Sam2` segment from point and box prompts, running each image's encoder
  once; `SigLip2` embeds images and texts into one cosine space. Every backend loads only a
  verified model and reads its input, normalisation and labels from the manifest, and the
  built-in manifest gains eleven Apache-2.0 records pinned by commit and SHA-256. Sessions
  run on the CPU unless CoreML or CUDA is chosen, and `onnx-dynamic` loads onnxruntime from
  `ORT_DYLIB_PATH`.
