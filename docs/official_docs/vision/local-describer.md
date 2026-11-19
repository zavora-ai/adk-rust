# Local Describer

Feature `mistralrs` runs a vision-language model in this process through `adk-mistralrs`:
`LlmDescriber::mistralrs` loads Qwen3-VL from weights verified against the built-in manifest
and answers descriptions, structured values and regions without an image leaving the machine.
The model's `Llm::data_locality()` is `InProcess`, so the describer takes `SentImage`s, never
reaches the egress gate, and the image privacy guardrail does not guard it.

> **Note:** `adk-vision` is **Experimental** (spec 3.7, §7.6). mistral.rs 0.8 loads Qwen3-VL
> from safetensors only; it has no GGUF path for the vision tower.

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["mistralrs"] }
adk-mistralrs = "3.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| Feature | What it adds |
|---------|--------------|
| `mistralrs` | `LlmDescriber::mistralrs` and the `mistralrs` module; implies `cloud` and `fetch` |
| `mistralrs-metal` | The same on Apple GPUs through Metal |
| `mistralrs-cuda` | The same on NVIDIA GPUs; needs the CUDA toolkit |

## Built-In Model

| Manifest | Model | Files | Licence |
|----------|-------|-------|---------|
| `qwen3-vl-2b-instruct` (`mistralrs::QWEN3_VL_2B`) | `Qwen/Qwen3-VL-2B-Instruct` at `89644892` | 10 files, 4.27 GB; `model.safetensors` is 4,255,140,312 bytes | Apache-2.0 |

Every file is pinned by SHA-256 and checked before mistral.rs reads the directory, and a
`*.safetensors` file in that directory the manifest does not pin refuses the load. A manifest
of your own with `"runtime": {"kind": "mistralRs", "architecture": "..."}` loads the same way.

## Describing an Image

```rust
use std::sync::Arc;

use adk_mistralrs::MistralRsConfig;
use adk_vision::mistralrs::QWEN3_VL_2B;
use adk_vision::{
    DecodeLimits, DescribeSpec, FetchConfig, InMemoryAcceptanceStore, LlmDescriber, ManifestSet,
    ModelFetcher, SourceImage, VisionCx, VisionRuntime,
};

#[tokio::main]
async fn main() -> adk_vision::Result<()> {
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let manifest = ManifestSet::builtin()?.get(QWEN3_VL_2B).cloned().expect("built in");
    let model = fetcher.fetch(&manifest).await?;
    let describer = LlmDescriber::mistralrs(&model, MistralRsConfig::default()).await?;

    let runtime = VisionRuntime::builder().describer(Arc::new(describer)).build()?;
    let bytes = std::fs::read("street.jpg").expect("a photo");
    let photo = SourceImage::decode(&bytes, &DecodeLimits::default())?;
    let spec = DescribeSpec::new("Where is every person?").with_regions();
    let answer = runtime.describe(&VisionCx::new(), &[&photo], &spec).await?;
    for region in &answer.value.regions {
        // Source pixels of the photo, with the frame and transforms on record.
        println!("{:?} {:?}", region.label, region.shape.bbox());
    }
    Ok(())
}
```

`MistralRsConfig` carries the device, in-situ quantisation (`isq`) and context settings; the
model source is always the verified directory.

## Regions

Qwen3-VL grounds objects as `bbox_2d` `[x1, y1, x2, y2]` on 0–1000 of the image, origin
top-left, whatever the image's size: Qwen's grounding cookbook maps a box back with
`value / 1000 × width`. That is `CoordFrame::Normalized { scale: 1000, order: Xy }`, the
frame of `Normalized1000::qwen3_vl`, so the prompt restates the frame the model was trained
on. Qwen2.5-VL answered in absolute pixels of its resized input instead; it has no adapter,
and a describer over it answers in text and values only.

| Behaviour | Value |
|-----------|-------|
| Response schema | None — `adk-mistralrs` passes none to the model; the prompt fixes the shape, and a bare list in Qwen's own form is read too |
| Image tokens | One per 32×32 tile (patch 16, merge 2) |
| Input limit | `mistralrs::MAX_PIXELS`, 1,048,576 pixels; the runtime downscales a larger image and records the resize. Raise it with `with_limits` |
| Cost | Zero: the model is in process, and each call records its units at zero cost |

A model already loaded with `MistralRsVisionModel` describes through `LlmDescriber::local`,
which picks the Qwen3-VL adapter from a Hugging Face model id such as
`Qwen/Qwen3-VL-2B-Instruct`; for a model loaded from a local path, give it with
`with_adapter(Arc::new(Normalized1000::qwen3_vl(id)))`.

## Related

- [Cloud vision backends](cloud.md) — `LlmDescriber` over remote models and the region adapters
- [Vision overview](index.md)
- [Local ONNX backends](onnx.md)
- [mistral.rs](../models/mistralrs.md)
