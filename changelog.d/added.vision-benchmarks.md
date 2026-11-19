- **Vision benchmarks** (`adk-bench`; spec 3.7 §7.17, X14): feature `vision` adds a harness
  that reports, per `adk-vision` backend, cold load, p50 and p95 per image at three input
  sizes, throughput, peak RSS, accuracy on the vision eval sets and cost per 1,000 images,
  with the machine, OS, CPU and runtime versions; a `vision` task-quality adapter; and the
  `vision_bench` example, which runs the benchmark or serves one backend to `cargo adk eval`.
  Features `vision-onnx`, `vision-coreml`, `vision-ocr`, `vision-ocr-vl`,
  `vision-ocr-vl-metal` and `vision-cloud` choose the backends; cloud rows replay recorded
  responses through the real clients. The nightly tier runs it on Apple Silicon and Linux
  CPU and publishes the table as an artifact; the first results are on the vision
  benchmarks page.
