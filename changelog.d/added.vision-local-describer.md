- **Qwen3-VL describer on the device** (`adk-vision`, feature `mistralrs`):
  `LlmDescriber::mistralrs` loads a vision-language model verified against its manifest into
  this process through `adk-mistralrs`, `InProcess`, so its images never reach the egress gate.
  The built-in manifest pins `Qwen/Qwen3-VL-2B-Instruct` (`qwen3-vl-2b-instruct`, Apache-2.0),
  and `Normalized1000::qwen3_vl` reads its regions in the model's own 0–1000 `bbox_2d` frame;
  `adapter_for_model` picks it for `qwen3-vl-*` model ids. `mistralrs-metal` and
  `mistralrs-cuda` run it on a GPU.
