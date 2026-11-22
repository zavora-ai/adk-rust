- **Local video questions that find their moment** (`adk-vision`, spec 3.7 S13):
  `FramesAnalyzer::with_visibility_pass` takes a `VisibilityPass`, which checks frames one
  by one (or a batch at a time, every `stride`-th frame) for what the question is about, then
  asks the question only over the frames around the first one that shows it, dismissing a
  positive the question does not confirm. The built-in manifest pins Qwen3-VL 4B Instruct
  (`mistralrs::QWEN3_VL_4B`, the local describer default) and 8B Instruct
  (`mistralrs::QWEN3_VL_8B`) for mistral.rs, every safetensors shard by revision and SHA-256.
- **Video analyzer conformance** (`adk-vision`, feature `conformance`):
  `video_analyzer_conformance_tests!` runs a `VideoAnalyzer` through `VisionRuntime` for
  frame metering, egress, event times and track regions, and the refusals: an unsupported
  request, cancellation, the kill switch, a frame cap and a remote analyzer without an egress
  gate. `FramesAnalyzer`, `GeminiVideoAnalyzer` and `SamVideoTracker` pass it.
