# Local ONNX Backends

Feature `onnx` runs detection, segmentation and embedding on the device through ONNX
Runtime (`ort`): RF-DETR finds objects, EfficientSAM and SAM 2.1 segment them from points
and boxes, and SigLIP 2 puts images and texts in one space for retrieval. No image leaves
the machine, every weight file is verified against its manifest before it loads, and every
answer reaches the caller in source pixels.

> **Note:** `adk-vision` is **Experimental** (spec 3.7, §7.6). The built-in records name
> Apache-2.0 exports only; the numbers on this page come from the crate's live tests, not
> yet from the benchmark harness.

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["onnx"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| Feature | What it adds |
|---------|--------------|
| `onnx` | The backends below; the onnxruntime binary is downloaded at build time over rustls. Implies `fetch` |
| `onnx-dynamic` | The same backends against an onnxruntime library loaded at run time from `ORT_DYLIB_PATH`, for offline and production builds |
| `onnx-coreml` | `OnnxExecutionProvider::CoreMl` (links only on Apple targets) |
| `onnx-cuda` | `OnnxExecutionProvider::Cuda` (downloads the CUDA build of onnxruntime) |
| `sam3-local` | `Sam3` and `Sam3Tracker`: Meta's SAM 3 from weights you supply under the SAM License, after you accept it ([SAM 3 Under the SAM License](#sam-3-under-the-sam-license)). In no `adk-rust` tier. Implies `onnx` |

## Backends

| Backend | Trait | Takes | Answers with | Answers in |
|---------|-------|-------|--------------|------------|
| `RfDetr` | `Detector` | `DetectQuery::Classes` (empty for every class) | Boxes; instance masks from an RF-DETR-Seg export | `ModelInput` with the manifest's letterbox |
| `EfficientSam` | `Segmenter` | Point and box prompts | One mask per object, scored by predicted IoU | `SentPixels` (images over 1024 px are downscaled first) |
| `Sam2` | `Segmenter` | Point and box prompts | One mask per object, scored by predicted IoU | `ModelInput` 1024×1024, stretched |
| `SigLip2` | `Embedder` | Images and texts | Unit-length vectors, `Metric::Cosine` | — |
| `Sam3` (`sam3-local`) | `Segmenter`, `Detector` | Concept prompts and `DetectQuery`; point and box prompts with `Sam3::with_tracker` | Masks (segmenting) or boxes (detecting), labelled with the concept | `ModelInput` 1008×1008, stretched |
| `Sam3Tracker` (`sam3-local`) | `Segmenter` | Point and box prompts | One mask per object, scored by predicted IoU | `ModelInput` 1008×1008, stretched |
| `YuNet` | `Detector` | `DetectQuery::Classes` of its one label, `face` | Face boxes, for the privacy redactors only | `SentPixels` (run at the full 640×640 input and at half of it) |

Each backend is built from a `VerifiedModel`, so a file loads only after its size and
SHA-256 matched the manifest (V-I10). It reads its input size, letterbox, normalisation and
labels from the manifest's `io`, so a user's own export runs through the same backend.
Inference runs on a blocking thread: a call does not stall the async runtime, and the
runtime's cancellation ends it at once. Each backend is `Locality::InProcess`, so the
runtime meters its images at zero cost and the egress recorder stays empty.

## Built-In Models

Every record pins a Hugging Face commit, each file's SHA-256 and size, and the licence;
`ManifestSet::builtin()` holds them and `ModelFetcher` downloads and verifies them.

| Id | Backend | Files | Repository @ commit | Licence | Live-tested |
|----|---------|-------|---------------------|---------|-------------|
| `rf-detr-n` | `RfDetr` | `onnx/model.onnx`, 108 MB | `onnx-community/rfdetr_nano-ONNX` @ `eae21cee` | Apache-2.0 | Yes |
| `rf-detr-n-q4f16` | `RfDetr` | `onnx/model_q4f16.onnx`, 19 MB | `onnx-community/rfdetr_nano-ONNX` @ `eae21cee` | Apache-2.0 | Yes |
| `rf-detr-s` | `RfDetr` | `onnx/model.onnx`, 115 MB, 512 px | `onnx-community/rfdetr_small-ONNX` @ `63463b68` | Apache-2.0 | Record only |
| `rf-detr-m` | `RfDetr` | `onnx/model.onnx`, 121 MB, 576 px | `onnx-community/rfdetr_medium-ONNX` @ `8553cefd` | Apache-2.0 | Record only |
| `rf-detr-seg-preview` | `RfDetr` (masks) | `rf-detr-seg-preview.onnx`, 134 MB, 432 px | `bukuroo/RF-DETR-Seg-ONNX` @ `627efc7f` (community export) | Apache-2.0 | Yes |
| `efficientsam-ti` | `EfficientSam` | encoder 25 MB, decoder 17 MB | `yunyangx/EfficientSAM` @ `1cf49585` | Apache-2.0 | Yes |
| `sam2.1-hiera-tiny` | `Sam2` | encoder and decoder with external data, 155 MB | `onnx-community/sam2.1-hiera-tiny-ONNX` @ `814a0666` | Apache-2.0 | Record only |
| `sam2.1-hiera-tiny-q4` | `Sam2` | the 4-bit encoder and decoder, 51 MB | `onnx-community/sam2.1-hiera-tiny-ONNX` @ `814a0666` | Apache-2.0 | Yes |
| `siglip2-base-patch16-256` | `SigLip2` | vision 372 MB, text 1.13 GB, tokenizer | `onnx-community/siglip2-base-patch16-256-ONNX` @ `d1114256` | Apache-2.0 | Record only |
| `siglip2-base-patch16-256-quantized` | `SigLip2` | vision q4 63 MB, text int8 283 MB, tokenizer | `onnx-community/siglip2-base-patch16-256-ONNX` @ `d1114256` | Apache-2.0 | Yes |
| `siglip2-so400m-patch16-256-quantized` | `SigLip2` | vision q4 278 MB, text int8 711 MB, tokenizer (1,152 dimensions, opt-in) | `onnx-community/siglip2-so400m-patch16-256-ONNX` @ `7881d8f2` | Apache-2.0 | Record only |
| `yunet-2023mar` | `YuNet` | `face_detection_yunet_2023mar.onnx`, 232 KB, 640×640 top-left letterbox | `opencv/face_detection_yunet` @ `3cc26e7f` | MIT | Yes |

RF-DETR's XL and 2XL checkpoints are PML 1.0 and are not built in. The RF-DETR records
list COCO's 91 label columns, the 80 named classes among them.

## Detecting Objects

```rust,no_run
use std::sync::Arc;

use adk_vision::onnx::{OnnxOptions, RfDetr};
use adk_vision::{
    DecodeLimits, DetectQuery, FetchConfig, InMemoryAcceptanceStore, ManifestSet,
    ModelFetcher, SourceImage, VisionCx, VisionRuntime,
};

#[tokio::main]
async fn main() -> adk_vision::Result<()> {
    let manifest = ManifestSet::builtin()?.get("rf-detr-n").cloned().expect("built in");
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let detector = RfDetr::new(&fetcher.fetch(&manifest).await?, &OnnxOptions::default())?;
    let runtime = VisionRuntime::builder().detector(Arc::new(detector)).build()?;

    let bytes = std::fs::read("kitchen.jpg").expect("a photo");
    let photo = SourceImage::decode(&bytes, &DecodeLimits::default())?;
    let query = DetectQuery::Classes(vec!["cat".into(), "laptop".into()]);
    let found = runtime.detect(&VisionCx::new(), &photo, &query).await?;
    for region in &found.value {
        println!("{:?} {:?} {:?}", region.label, region.score, region.shape.bbox());
    }
    Ok(())
}
```

The graph answers 300 queries; every query-class pair whose sigmoid score reaches the
threshold (0.5, `with_threshold` to change it) is a detection, best first, as RF-DETR's
reference post-processing does. A set predictor needs no NMS. A class name the model does
not know is an `InvalidInput` naming some it does; `DetectQuery::Concepts` is
`Unsupported`, because RF-DETR is closed-set.

## Segmenting from Points and Boxes

```rust,no_run
use std::sync::Arc;

use adk_vision::onnx::{OnnxOptions, Sam2};
use adk_vision::{
    BBox, DecodeLimits, FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher,
    SegmentPrompt, Shape, SourceImage, VisionCx, VisionRuntime,
};

#[tokio::main]
async fn main() -> adk_vision::Result<()> {
    let manifest = ManifestSet::builtin()?.get("sam2.1-hiera-tiny-q4").cloned().expect("built in");
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let segmenter = Sam2::new(&fetcher.fetch(&manifest).await?, &OnnxOptions::default())?;
    let runtime = VisionRuntime::builder().segmenter(Arc::new(segmenter)).build()?;

    let bytes = std::fs::read("desk.jpg").expect("a photo");
    let photo = SourceImage::decode(&bytes, &DecodeLimits::default())?;
    // Two objects: the mug's box with a negative point on its handle, and the lamp's point.
    let prompts = [
        SegmentPrompt::Box { bbox: BBox::new(120.0, 300.0, 260.0, 470.0) },
        SegmentPrompt::Point { at: [245.0, 380.0], positive: false },
        SegmentPrompt::Point { at: [610.0, 140.0], positive: true },
    ];
    let masks = runtime.segment(&VisionCx::new(), &photo, &prompts).await?;
    for region in &masks.value {
        if let Shape::Mask(mask) = &region.shape {
            println!("{} pixels, predicted IoU {:?}", mask.area(), region.score);
        }
    }
    Ok(())
}
```

Prompts become objects by one rule, for both segmenters:

| Prompts | Objects |
|---------|---------|
| Each `Box` | One object, refined by the points inside it (the first box containing a point takes it) |
| `Point`s outside every box | One more object together; at least one must be positive |
| `Concept`, `Mask` | `Unsupported`: route concept prompts to a concept segmenter such as `sam-3.1` |

The encoder runs once per prepared image and its embedding is kept for the next prompts on
the same image, so the decoder alone runs for each later call. Of the three masks the
decoder proposes, the one with the highest predicted IoU is kept. SAM 2 yields no region
for an object it scores as absent.

## Embedding Images and Texts

```rust,no_run
use std::sync::Arc;

use adk_vision::onnx::{OnnxOptions, SigLip2};
use adk_vision::{
    CallOptions, DecodeLimits, FetchConfig, InMemoryAcceptanceStore, ManifestSet,
    ModelFetcher, SourceImage, VisionCx, VisionRuntime,
};

#[tokio::main]
async fn main() -> adk_vision::Result<()> {
    let manifest = ManifestSet::builtin()?
        .get("siglip2-base-patch16-256-quantized")
        .cloned()
        .expect("built in");
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let embedder = SigLip2::new(&fetcher.fetch(&manifest).await?, &OnnxOptions::default())?;
    let runtime = VisionRuntime::builder().embedder(Arc::new(embedder)).build()?;

    let mut photos = Vec::new();
    for name in ["bear.jpg", "cat.jpg", "sheep.jpg"] {
        let bytes = std::fs::read(name).expect("a photo");
        photos.push(SourceImage::decode(&bytes, &DecodeLimits::default())?);
    }
    let cx = VisionCx::new();
    let options = CallOptions::default();
    let images = runtime.embed_images(&cx, &photos.iter().collect::<Vec<_>>(), &options).await?;
    let query = runtime.embed_texts(&cx, &["a photo of a cat"], &options).await?;

    // Unit-length vectors: the dot product is the cosine.
    let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    for (name, image) in ["bear", "cat", "sheep"].iter().zip(&images.value) {
        println!("{name}: {:.3}", cosine(&query.value[0], image));
    }
    Ok(())
}
```

Texts are lowercased, as SigLIP 2 was trained, tokenised with the Gemma tokenizer, ended
with `<eos>` and padded to 64 tokens. The space names the manifest id and revision, so an
index never mixes vectors from two models.

## Your Own Export

A fine-tuned RF-DETR, or any export of these architectures, runs through the same backend
with a manifest of its own. A `local` source points at the directory holding the files:

```json
{
  "id": "rf-detr-n-shelves",
  "task": "detect",
  "source": { "kind": "local", "dir": "/models/rf-detr-n-shelves" },
  "files": [{
    "path": "inference_model.onnx",
    "sha256": "sha256:<the file's SHA-256>",
    "size": 108074865
  }],
  "licence": {
    "id": "Apache-2.0",
    "url": "https://github.com/roboflow/rf-detr/blob/develop/LICENSE",
    "class": "permissive"
  },
  "runtime": { "kind": "ort", "opset": 17 },
  "io": {
    "input": { "width": 384, "height": 384, "letterbox": "stretch" },
    "mean": [0.485, 0.456, 0.406],
    "std": [0.229, 0.224, 0.225],
    "labels": ["", "empty-slot", "price-tag", "product"]
  }
}
```

| Backend | Files the manifest pins | `io` it reads |
|---------|-------------------------|---------------|
| `RfDetr` | Exactly one `.onnx` file | `input` and `labels` (required; one per logit column, empty for an unused column), `mean` and `std` |
| `EfficientSam` | The `.onnx` file whose name contains `encoder`, and the one containing `decoder` | `input` (the long edge the runtime downscales to) |
| `Sam2` | The `.onnx` file containing `encoder` but not `decoder`, the one containing `decoder`, and their `.onnx_data` beside them | `input`, `mean`, `std` |
| `SigLip2` | The `.onnx` file containing `vision`, the one containing `text`, and `tokenizer.json` | `input`, `mean`, `std` |
| `Sam3` | The `.onnx` file containing `vision`, the one containing `text`, the one containing `decoder`, and `tokenizer.json`; licence `LicenseRef-SAM`, `useRestricted` | `input`, `mean`, `std` (1008×1008, stretch and 0.5 when absent) |
| `Sam3Tracker` | As `Sam2`; licence `LicenseRef-SAM`, `useRestricted` | As `Sam3` |
| `YuNet` | Exactly one `.onnx` file with `cls_<s>`, `obj_<s>` and `bbox_<s>` outputs for strides 8, 16 and 32 | `input` and exactly one label; it takes BGR values from 0 to 255, so `mean` and `std` are not read |

RF-DETR's graph may name its outputs `pred_boxes` and `logits` (the Transformers.js
export) or `dets` and `labels` (RF-DETR's own `export()`); an RF-DETR-Seg export's mask
output is found by name (`pred_masks`, `masks`) or as the other rank-4 output.

## SAM 3 Under the SAM License

Feature `sam3-local` runs Meta's SAM 3 on the device: `Sam3` segments and detects every
instance of a noun phrase, as the cloud `sam-3.1` does, and `Sam3Tracker` segments from
points and boxes. ADK-Rust ships the backends, not the model:

| Item | State |
|------|-------|
| Weights | Meta publishes SAM 3 and SAM 3.1 on Hugging Face ([`facebook/sam3`](https://huggingface.co/facebook/sam3), [`facebook/sam3.1`](https://huggingface.co/facebook/sam3.1)) behind an access request Meta approves by hand. The request asks for a name, date of birth, country, affiliation and job title, and its checkbox accepts the SAM License and Meta's Privacy Policy |
| ONNX | Meta publishes PyTorch and safetensors checkpoints only. `Sam3` reads the graph layout of the Transformers `Sam3Model`, `Sam3Tracker` that of Transformers.js's `Sam3TrackerModel` |
| Built-in record | None. A record would either pin Meta's gated checkpoint, which `ort` cannot run, or an export someone else redistributed, which would route every user around Meta's access request. You write the manifest for weights you obtained |
| Licence | The [SAM License](https://github.com/facebookresearch/sam3/blob/a13e358df479a80938c8bfed0e0754bea8fdb277/LICENSE) (`LicenseRef-SAM`), the text that also governs `adk-vision-sam-codec`. `Sam3::new` and `Sam3Tracker::new` refuse a manifest that names another licence or class, so the weights never load without a recorded acceptance |

> **Important:** the SAM License binds whoever uses or distributes SAM 3 weights or their
> exports: distribution only under the SAM License with a copy of it (clause 1.b.i);
> acknowledging SAM in published research (1.b.ii); no use for ITAR-controlled activities or
> end uses Trade Controls prohibit, including military, warfare, nuclear, espionage and
> weapons (1.b.iii, 1.b.v); no reverse engineering (1.b.iv); an indemnity of Meta for
> third-party claims from your use (5.b); and terms Meta may change (8). Exports others
> publish are SAM Materials too, under the same terms.

### Running Your Own Export

1. Request access to `facebook/sam3` and download the checkpoint once Meta approves it.
2. Export it to ONNX in the layout below: three graphs for concepts, two for the tracker.
3. Write a manifest per model, each file pinned by SHA-256 and size, naming the SAM License:

   ```json
   {
     "id": "sam3-local",
     "task": "segment",
     "source": { "kind": "local", "dir": "/models/sam3" },
     "files": [
       { "path": "vision_encoder.onnx", "sha256": "sha256:<its SHA-256>", "size": 0 },
       { "path": "text_encoder.onnx", "sha256": "sha256:<its SHA-256>", "size": 0 },
       { "path": "prompt_decoder.onnx", "sha256": "sha256:<its SHA-256>", "size": 0 },
       { "path": "tokenizer.json", "sha256": "sha256:<its SHA-256>", "size": 0 }
     ],
     "licence": {
       "id": "LicenseRef-SAM",
       "url": "https://github.com/facebookresearch/sam3/blob/a13e358df479a80938c8bfed0e0754bea8fdb277/LICENSE",
       "class": "useRestricted"
     },
     "runtime": { "kind": "ort", "opset": 18 }
   }
   ```

4. Read the licence and record your acceptance from a terminal; until then verification fails
   with `licence acceptance required` and the licence URL:

   ```bash
   adk-rust vision models accept sam3-local --manifest sam3-local.json
   ```

5. Load it with the acceptances of the project:

```rust,no_run
use std::path::Path;
use std::sync::Arc;
use adk_vision::onnx::{OnnxOptions, Sam3};
use adk_vision::{
    DecodeLimits, FetchConfig, FileAcceptanceStore, ManifestSet, ModelFetcher,
    SegmentPrompt, SourceImage, VisionCx, VisionRuntime,
};

#[tokio::main]
async fn main() -> adk_vision::Result<()> {
    let project = Path::new(".");
    let manifests = ManifestSet::new().with_dir(Path::new("manifests"))?;
    let acceptances = Arc::new(FileAcceptanceStore::in_project(project));
    let fetcher = ModelFetcher::new(FetchConfig::from_env(), acceptances);
    let model = fetcher.fetch(manifests.get("sam3-local").expect("your manifest")).await?;
    let sam3 = Arc::new(Sam3::new(&model, &OnnxOptions::default())?);

    let runtime = VisionRuntime::builder().segmenter(sam3).build()?;
    let bytes = std::fs::read("shelf.jpg").expect("a photo");
    let photo = SourceImage::decode(&bytes, &DecodeLimits::default())?;
    let prompts = [SegmentPrompt::Concept { text: "torn seal".into() }];
    for region in runtime.segment(&VisionCx::new(), &photo, &prompts).await?.value {
        println!("{:?} {:?} {:?}", region.label, region.score, region.shape.bbox());
    }
    Ok(())
}
```

`Sam3::with_tracker(Sam3Tracker::new(&tracker_model, &options)?)` adds point and box prompts to
the same segmenter, and `Sam3::with_threshold` changes the score cut-off.

### Graph Layouts

| Graph (file name contains) | Inputs | Outputs |
|----------------------------|--------|---------|
| `vision` | `pixel_values` `[1, 3, 1008, 1008]` | `fpn_hidden_state_0`, `_1`, `_2` |
| `text` | `input_ids`, `attention_mask` `[1, 32]` (int64) | `text_features` `[1, 32, 256]` |
| `decoder` | the three feature maps, `text_features`, `attention_mask` | `pred_masks` `[1, Q, 288, 288]` (logits), `pred_boxes` `[1, Q, 4]` (corners in `0..1`), `pred_logits` `[1, Q]`, `presence_logits` `[1, 1]` |
| Tracker `encoder` | `pixel_values` | `image_embeddings.0`, `.1`, `.2` |
| Tracker `decoder` | `input_points`, `input_labels`, `input_boxes` and the embeddings | `iou_scores`, `pred_masks` (288×288), `object_score_logits` |

The image is stretched to 1008×1008 and mapped to `[-1, 1]`; concepts are lowercased CLIP
tokens padded with `<|endoftext|>` to 32. A query scores `sigmoid(pred_logits) ×
sigmoid(presence_logits)` and is kept above 0.5, as Meta's `Sam3Processor` keeps it; its mask
is upsampled bilinearly to 1008×1008 and thresholded at 0. The image encoder runs once per
prepared image and the text encoder once per concept, both cached for the next calls.

Stand-in graphs with these inputs and outputs run the licence gate, the coordinate mapping
and the conformance suite in CI (`adk-vision/tests/sam3_local_tests.rs`). Accuracy and
latency against real weights are not recorded yet: `sam3_local_live_tests` measures them on
the drawn shapes and the S5 widgets beside SAM 2.1 Tiny, once weights and an acceptance are in
place:

```bash
ADK_VISION_SAM3_MANIFEST=sam3-local.json ADK_VISION_SAM3_PROJECT=. \
  cargo nextest run -p adk-vision --features sam3-local --test sam3_local_live_tests \
  --run-ignored only --no-capture
```

## Execution Providers

```rust
use adk_vision::onnx::{OnnxExecutionProvider, OnnxOptions};

let options = OnnxOptions::default()
    .with_execution_provider(OnnxExecutionProvider::CoreMl)
    .with_intra_threads(4);
assert_eq!(options.execution_provider, OnnxExecutionProvider::CoreMl);
```

| `OnnxExecutionProvider` | Needs | Behaviour |
|-------------------------|-------|-----------|
| `Cpu` (default) | — | No accelerator is registered and onnxruntime picks no device itself: the workspace turns on no `ort` API level of 22 or more, from which `ort` asks onnxruntime to pick a device for efficiency, CoreML on Apple silicon |
| `CoreMl` | `onnx-coreml` | CoreML takes the nodes it supports; the CPU runs the rest |
| `Cuda` | `onnx-cuda` | CUDA takes the nodes it supports; the CPU runs the rest |

A provider this build does not compile logs a warning and runs on the CPU.

> **Note:** On an Apple M4, CoreML ran every built-in export slower than the CPU (RF-DETR
> Nano 474 ms against 56 ms per image), because these exports have dynamic shapes. SAM 2's
> point-only prompts (an empty box tensor) and the 4-bit SigLIP 2 vision tower fail under
> CoreML. Use the CPU on Apple silicon for these exports.

## Offline and Production Builds

`onnx-dynamic` links no onnxruntime at build time; set `ORT_DYLIB_PATH` to the library at
run time. With `HF_HUB_OFFLINE=1`, the fetcher opens no socket and verifies the cached
files before every load:

```bash
ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.dylib \
HF_HUB_CACHE=/var/lib/models HF_HUB_OFFLINE=1 \
  ./my-agent
```

The crate's live tests pass against onnxruntime 1.31 (the `ort` download); with `ort`
2.0.0-rc.13 they also passed against onnxruntime 1.32 loaded this way.

## One ONNX Runtime Environment

`ort` allows one ONNX Runtime environment per process. `adk-vision`, `adk-audio` and
`adk-rag` take theirs from `adk_core::onnx::ort_environment`, which builds it with `ort`'s
defaults on first use, and `adk-vision` hands the same one to `oar-ocr`. An application that
wants its own environment, for one thread pool across every model or its own logger, installs
it before the first model loads:

```toml
[dependencies]
adk-core = { version = "3.0.0", features = ["ort"] }
adk-vision = { version = "3.0.0", features = ["onnx"] }
ort = { version = "=2.0.0-rc.15", default-features = false, features = ["std"] }
```

```rust
use ort::environment::GlobalThreadPoolOptions;

fn main() -> ort::Result<()> {
    let threads = GlobalThreadPoolOptions::default().with_intra_threads(4)?;
    let environment = ort::init().with_global_thread_pool(threads).build()?;
    adk_core::onnx::set_ort_environment(environment)?;
    // Every session ADK-Rust and oar-ocr create from here on runs on this pool.
    Ok(())
}
```

| Situation | Result |
|-----------|--------|
| Nothing installed | The first model load builds the environment; it lives until the process exits |
| `set_ort_environment` after a model loaded, or called twice | An error: the environment is already installed |
| Other code built an environment first and did not install it | Loading a model fails with an error that names `set_ort_environment` |

> **Note:** Keep `ort`'s `api-*` features below 22 in a build that runs ADK-Rust's ONNX models,
> `ort`'s own default features included. From 22, `ort` lets onnxruntime pick each session's
> device; on Apple silicon it picks CoreML for PP-OCRv6, which then reads at about half the speed
> with two to three times the memory, and `oar-ocr` sessions cannot be pinned back to the CPU.

## Measured Results

From the crate's live tests (`adk-vision/tests/onnx_live_tests.rs`) on an Apple M4 (10
cores, 24 GB, macOS 26.2), onnxruntime 1.28 on the CPU, one image at a time:

| Model | Fixture | Accuracy | Latency |
|-------|---------|----------|---------|
| `rf-detr-n` | Three COCO 2017 validation photos, seven ground-truth objects | Every object found; box IoU 0.930–0.995, scores 0.715–0.973 | 58 ms p50 per image |
| `rf-detr-n-q4f16` | The same | Every object found from threshold 0.3; IoU 0.855–0.969, scores 0.426–0.888 | 137 ms p50 |
| `rf-detr-seg-preview` | The same | Every object's mask, bounding-box IoU 0.909–0.988 | 229 ms p50 |
| `efficientsam-ti` | A drawn disc and rectangle, 640×480 | Mask IoU 0.978–0.992 from boxes and a point | 633 ms first call, 21 ms per later prompt |
| `sam2.1-hiera-tiny-q4` | The same | Mask IoU 0.997–0.9996 | 1.10 s first call, 50 ms per later prompt |
| `siglip2-base-patch16-256-quantized` | Three COCO photos, three captions | Each caption ranks its photo first, and each photo its caption | 445 ms for three images, 34 ms for three texts |
| `yunet-2023mar` | Drawn portraits (`stream::fixtures::draw_portrait`) 60 to 1,400 px tall in 640×360 and 1280×720 frames | Every face found, scores 0.64–0.91; none found again after the face blur | 38–41 ms per 640×360 frame and 63–82 ms per 1280×720 frame, both passes, onnxruntime 1.31 |

The 4-bit RF-DETR export scores the same objects lower than the full-precision one (the bear
at 0.43 against 0.97), so `rf-detr-n` is the default in the examples.

On onnxruntime 1.31 (`ort` 2.0.0-rc.15) every accuracy column above is unchanged except
`rf-detr-n-q4f16`'s, which reads IoU 0.853–0.969 and scores 0.423–0.888.

## Related

- [Vision overview](index.md)
- [Spec 3.7](../../design/spec-3.7-vision.md)
