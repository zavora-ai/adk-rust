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

## Built-In Models

| Manifest | Model | Files | Licence |
|----------|-------|-------|---------|
| `qwen3-vl-2b-instruct` (`mistralrs::QWEN3_VL_2B`) | `Qwen/Qwen3-VL-2B-Instruct` at `89644892` | 10 files, 4.27 GB; `model.safetensors` is 4,255,140,312 bytes | Apache-2.0 |
| `qwen3-vl-4b-instruct` (`mistralrs::QWEN3_VL_4B`) | `Qwen/Qwen3-VL-4B-Instruct` at `ebb281ec` | 12 files, 8.89 GB; two safetensors shards and their index | Apache-2.0 |
| `qwen3-vl-8b-instruct` (`mistralrs::QWEN3_VL_8B`) | `Qwen/Qwen3-VL-8B-Instruct` at `0c351dd0` | 14 files, 17.5 GB; four safetensors shards and their index | Apache-2.0 |

The 4B model is the local describer the spec defaults to, and the 8B model the local GUI
grounding default. The weights load in bf16, so a model needs about its download size in
memory; `MistralRsConfig::isq` quantises in place at load time to need less.

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

## Measured

`Qwen/Qwen3-VL-2B-Instruct` on an Apple M4 CPU (10 cores, 24 GB), F16, the `dev` profile with
dependencies at `opt-level = 1`, `MistralRsConfig::default()`; from the crate's live tests
(`adk-vision/tests/mistralrs_tests.rs`):

| Run | Result | Time |
|-----|--------|------|
| `LlmDescriber::mistralrs` after the fetcher verified the files | loaded, `InProcess` | 2.4–9.9 s |
| Conformance shapes, Qwen's own grounding prompt | `bbox_2d` `[127, 144, 398, 420]` and `[558, 514, 868, 874]`: IoU 0.79 and 0.80 read as 0–1000, 0.09 and 0.00 read as pixels | 29–35 s |
| Conformance shapes through the describer | IoU 0.83 (red) and 0.80 (blue) in source pixels | 19 s |
| COCO 2017 val `000000000785`, "Give a box for every person" | "the person is skiing"; person box IoU 0.985 against COCO's ground truth | 28 s |

A release build, `isq` quantisation or `mistralrs-metal` shortens the calls; they were not
measured here.

## On Ollama

A vision model served by Ollama on this machine is a second local route (spec 3.7 X19):
`LlmDescriber::local` over adk-model's `OllamaModel`. Its `Llm::data_locality()` is `Loopback`
at a loopback host, so the describer takes `SentImage`s and nothing passes the egress gate; a
cloud model (`…-cloud`) or a server on another machine is `Remote`, and `LlmDescriber::local`
refuses it. The image goes in the message's native `images`, so the model sees it.

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["cloud"] }
adk-model = { version = "3.0.0", default-features = false, features = ["ollama"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use std::sync::Arc;

use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_vision::{DecodeLimits, DescribeSpec, LlmDescriber, SourceImage, VisionCx, VisionRuntime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `ollama pull qwen3-vl:4b` first; `ollama show qwen3-vl:4b` lists `vision`.
    let model = OllamaModel::new(OllamaConfig::with_host("http://127.0.0.1:11434", "qwen3-vl:4b"))?;
    // A thinking model reasons before it answers: leave room for both.
    let describer = LlmDescriber::local(Arc::new(model))?.with_max_output_tokens(1_500);
    let runtime = VisionRuntime::builder().describer(Arc::new(describer)).build()?;

    let bytes = std::fs::read("shelf.jpg")?;
    let photo = SourceImage::decode(&bytes, &DecodeLimits::default())?;
    let spec = DescribeSpec::new("What is on the shelf?");
    let answer = runtime.describe(&VisionCx::new(), &[&photo], &spec).await?;
    println!("{}", answer.value.text.unwrap_or_default());
    Ok(())
}
```

A `qwen3-vl…` model name picks the Qwen3-VL adapter, so regions come back in its 0–1000 frame
as over mistral.rs. Ollama 0.32 reads PNG, JPEG and GIF from `images` and refuses WebP; the
runtime sends PNG.

| Run | Result | Time |
|-----|--------|------|
| `qwen3-vl:4b` (Q4_K_M) on Ollama 0.32.13, Apple M4, the conformance shapes, "What coloured shapes are on this image?" | "The colored shapes on the image are red and blue." (`adk-vision/tests/ollama_describer_tests.rs`) | 14.1 s with the model loaded |

## Related

- [Cloud vision backends](cloud.md) — `LlmDescriber` over remote models and the region adapters
- [Vision overview](index.md)
- [Local ONNX backends](onnx.md)
- [mistral.rs](../models/mistralrs.md)
- [Ollama](../models/ollama.md)
