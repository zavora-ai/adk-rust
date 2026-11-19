- **`adk-rust` features `vision` and `vision-local`**: `vision` (in `full`) re-exports
  `adk-vision` as `adk_rust::vision` with the fetcher, the cloud describers, NVIDIA NIM, Meta
  `sam-3.1` and the image privacy guardrail, and turns on the harness's vision pack and, with
  `cli`, `adk-rust vision`; `vision-local` (in no tier) adds the on-device backends on ONNX
  Runtime and Candle. The feature surface is 154 features.
