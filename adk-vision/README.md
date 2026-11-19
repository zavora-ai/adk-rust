# adk-vision

Local-first perception for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-vision.svg)](https://crates.io/crates/adk-vision)
[![Documentation](https://docs.rs/adk-vision/badge.svg)](https://docs.rs/adk-vision)
[![License](https://img.shields.io/crates/l/adk-vision.svg)](../LICENSE)

> **Note:** `adk-vision` is **Experimental** (spec 3.7). This release holds the contracts —
> media, regions, manifests, traits, the runtime, the egress boundary and the conformance
> suite, the local ONNX backends (`onnx`), and the cloud backends: Gemini, Claude, Muse and
> NVIDIA NIM describers (`cloud`, `nim`) and Meta `sam-3.1` (`sam`). OCR and mistral.rs
> backends arrive as features in later releases.

## Overview

`adk-vision` gives an agent sight through contracts first:

| Contract | Type | What it guarantees |
|----------|------|--------------------|
| Media | `ImageRef`, `Frame`, `VideoRef`, `MediaStore` | Pixels enter once and are stored as artifacts by `sha256:` digest; the digest is what travels |
| Regions | `Region`, `RegionSet`, `CoordFrame`, `TransformChain` | Every region is in source-image pixels and records the frame its backend answered in and the transforms in between |
| Manifests | `ModelManifest`, `LicenceGate`, `VerifiedModel` | Every weight file is pinned by revision and SHA-256 and names its licence; nothing loads unverified |
| Traits | `Detector`, `Segmenter`, `DocumentReader`, `Embedder`, `Describer`, `Tracker`, `VideoAnalyzer`, `Editor` | One contract per perception task, each with a conformance suite |
| Runtime | `VisionRuntime` | Local-first routing, metering, cancellation, mapping to source pixels, crop escalation |
| Egress | `EgressGate`, `ReleasedImage` | A remote backend only ever receives an image the gate cropped, redacted, approved and recorded |
| Tools | `VisionToolset` | `vision_detect`, `vision_segment`, `vision_read`, `vision_describe`, `vision_crop`, `vision_redact`, `vision_annotate` on the governed path; a remote call is held with the exact egress image and releases only that digest |
| Privacy | `PersonRedactor`, `PersonalTextRedactor`, `ImagePrivacyGuardrail` | Faces and personal text filled solid black on the device; the same gate on model requests, tool results and user input |

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["fetch"] }
```

| Feature | Contents |
|---------|----------|
| (always) | media, geometry, manifests, the traits, `VisionRuntime`, the egress gate, the deterministic `Editor`, `VisionToolset`, `PersonRedactor`, the `vision.*` audit records |
| `fetch` | `ModelFetcher`: Hugging Face downloads by revision through `hf-hub` 0.5, SHA-256 checked before load, `HF_HUB_OFFLINE` honoured |
| `conformance` | The conformance suite and its per-trait test macros |
| `guardrail` | `ImagePrivacyGuardrail` (a `BeforeModelCallback`, an `adk-guardrail` `Guardrail` and `ToolGuardrail`) and `PersonalTextRedactor` over `PiiRedactor`'s byte ranges |
| `onnx` | Local backends on ONNX Runtime: `RfDetr`, `EfficientSam`, `Sam2`, `SigLip2`; onnxruntime downloaded at build time |
| `onnx-dynamic` | The same backends against an onnxruntime library loaded from `ORT_DYLIB_PATH` |
| `onnx-coreml`, `onnx-cuda` | The CoreML and CUDA execution providers for the `onnx` backends |
| `cloud` | `LlmDescriber`: any `adk_core::Llm` that takes images as a `Describer`, with Gemini (`box_2d`, 0–1000), Claude (pixels of the image as sent) and prompt-fixed 0–1000 region adapters, and provider-formula preflight estimates |
| `nim` | NVIDIA NIM vision-language models (`NimConfig`), reasoning off, unpriced; implies `cloud` |
| `sam` | `SamSegmenter`: Meta `sam-3.1` request building, stream parser and mask placement, billed per request at $2.50 per 1,000 images |

## Quick Start

A backend answers in its own frame; the runtime prepares the image it sees and maps the
answer back to source pixels:

```rust
use std::sync::Arc;
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, DetectQuery, Detector, InputLimits,
    Locality, RawRegion, SentImage, SourceImage, VisionCx, VisionRuntime,
};

/// Finds the whole image, in the pixels it was sent.
struct Whole(BackendDescriptor);

#[async_trait::async_trait]
impl Detector for Whole {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let (w, h) = image.size();
        Ok(vec![RawRegion::boxed(BBox::new(0.0, 0.0, w as f32, h as f32), "page", 0.99)])
    }
}

#[tokio::main]
async fn main() -> adk_vision::Result<()> {
    // The backend takes at most 500 px on the long edge; the runtime downscales for it.
    let whole = BackendDescriptor::new("whole", Locality::InProcess, CoordFrame::SentPixels)
        .with_limits(InputLimits::long_edge(500));
    let runtime = VisionRuntime::builder().detector(Arc::new(Whole(whole))).build()?;

    let page = SourceImage::from_pixels(DecodedImage::filled(2000, 1000, [255; 4]))?;
    let found = runtime
        .detect(&VisionCx::new(), &page, &DetectQuery::Classes(vec!["page".into()]))
        .await?;

    // In source pixels, with the resize on record.
    assert_eq!(found.value[0].shape.bbox(), Some(BBox::new(0.0, 0.0, 2000.0, 1000.0)));
    Ok(())
}
```

## Local Backends

Feature `onnx` runs RF-DETR detection (and RF-DETR-Seg instance masks), EfficientSAM-Ti
and SAM 2.1 Tiny point and box segmentation, and SigLIP 2 image and text embeddings on the
device. Each backend loads a `VerifiedModel` from the built-in manifest (eleven Apache-2.0
records pinned by commit and SHA-256) or your own, such as an RF-DETR fine-tune:

```rust,no_run
use std::sync::Arc;
use adk_vision::onnx::{OnnxOptions, RfDetr};
use adk_vision::{
    DecodeLimits, DetectQuery, FetchConfig, InMemoryAcceptanceStore, ManifestSet,
    ModelFetcher, SourceImage, VisionCx, VisionRuntime,
};

#[tokio::main]
async fn main() -> adk_vision::Result<()> {
    // RF-DETR Nano: 108 MB, fetched once into the Hugging Face cache and verified.
    let manifest = ManifestSet::builtin()?.get("rf-detr-n").cloned().expect("built in");
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let detector = RfDetr::new(&fetcher.fetch(&manifest).await?, &OnnxOptions::default())?;
    let runtime = VisionRuntime::builder().detector(Arc::new(detector)).build()?;

    let bytes = std::fs::read("street.jpg").expect("a photo");
    let photo = SourceImage::decode(&bytes, &DecodeLimits::default())?;
    let people = runtime
        .detect(&VisionCx::new(), &photo, &DetectQuery::Classes(vec!["person".into()]))
        .await?;
    for person in &people.value {
        println!("{:?} {:?}", person.score, person.shape.bbox());
    }
    Ok(())
}
```

See [Local ONNX backends](../docs/official_docs/vision/onnx.md) for the models, prompts,
execution providers and measured latency.

## Coordinate Frames

Providers answer in different frames. Each backend's descriptor names its `CoordFrame`, and
the runtime records every `Transform` between the source image and what the backend saw:

| Frame | Who answers in it | Mapped to source by |
|-------|-------------------|---------------------|
| `SentPixels` | Meta `sam-3.1`; Claude once the runtime resized to its limit; local backends that undo their own letterbox | the transform chain |
| `Normalized { scale: 1000, order: Yx }` | Gemini's `box_2d` (`[ymin, xmin, ymax, xmax]`) | scale to the sent size, then the chain |
| `ModelInput { width, height, letterbox }` | RF-DETR and SAM 2 tensors | undo the letterbox, then the chain |

`ScreenSpace` turns screenshot pixels into logical points for a click: a 3024×1964
screenshot of a display at device pixel ratio 2 is 1512×982 points. `RegionSet` is the wire
form, published as a JSON Schema in [`schema/region-set.json`](schema/region-set.json).

## Remote Backends and the Egress Gate

An on-device backend implements its trait for `SentImage`; a remote backend implements it
for `ReleasedImage`, so handing a remote backend an unreleased image does not compile. Only
`EgressGate::release` constructs a `ReleasedImage`, after six steps:

1. **Destination** — the adk-core `EgressPolicy` decides each host.
2. **Minimise** — crop to the region the call needs, downscale to the backend's limit.
3. **Redact** — every class in the policy (`faces`, `personal_text`, ...) is filled with
   solid colour by its `Redactor`; a class with no redactor refuses the release.
4. **Decide** — the rule for the host and data class says `deny`, `ask` or `allow`; `ask`
   holds until a person approves that exact egress digest.
5. **Record** — the receipt (source and egress digests, chain, redactions, decision) goes to
   the `EgressRecorder` before the image is returned.
6. **Release**.

The rules live in `.adk/vision.toml`; there is no setting that releases before redaction:

```toml
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "generativelanguage.googleapis.com"
classes = ["document_crop"]
action = "ask"
```

## Routing and Escalation

A route names a primary backend and an optional escalation. On a low score, no result, or
disagreement with a second on-device backend, the runtime crops the uncertain region plus a
margin and sends only that crop — never a whole image when a region will do. The escalated
answer replaces the primary's and records `escalatedFrom`:

```rust,ignore
use adk_vision::{Escalation, Route, Trigger, VisionTask};

let runtime = VisionRuntime::builder()
    .detector(local)
    .remote_detector(cloud)
    .egress_gate(gate)
    .route(
        VisionTask::Detect,
        Route::to("rf-detr-m").escalate(
            Escalation::to("sam-3.1")
                .on(Trigger::LowConfidence { threshold: 0.5 })
                .with_data_class("document_crop"),
        ),
    )
    .build()?;
```

Every call is metered against the context's `ModelCallScope`: images are recorded as
units, at the unit price for a per-unit backend and at zero cost for a local one. A frozen
kill switch refuses every call and every release.

## Model Manifests and Licences

A `ModelManifest` pins each file by Hugging Face commit and SHA-256 and names its licence:

| Licence class | Fetched and loaded when |
|---------------|-------------------------|
| `permissive` | always |
| `useRestricted` | a person's acceptance is recorded (`adk-rust vision models accept <id>`); no agent tool records one |
| `nonCommercial`, `copyleft` | the user's `LicencePolicy` names the model; never in the built-in manifest |

`ModelFetcher` (feature `fetch`) reads `HF_HUB_CACHE`, `HF_HOME`, `HF_TOKEN`, `HF_ENDPOINT`
and `HF_HUB_OFFLINE` by Python's conventions. With `HF_HUB_OFFLINE=1` it opens no socket. It
checks every file's size and SHA-256 after download and before every load, and refuses a
mismatch with both digests. A 401 or 403 is a gated repository and asks for licence
acceptance. Backends take a `VerifiedModel`, which only verification constructs.

## Conformance

Every backend runs the same suite through a `VisionRuntime`: coordinates in source pixels,
usage recorded, cancellation, the kill switch, no egress from local backends, oversized
input, and rate-limit mapping. Enable `conformance` in `[dev-dependencies]`:

```rust,ignore
mod my_detector {
    use std::sync::Arc;
    use adk_vision::conformance::DetectorTarget;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Local(Arc::new(my_crate::MyDetector::new())),
    }
}
```

## Documentation

- [Vision overview](../docs/official_docs/vision/index.md)
- [Governed vision tools](../docs/official_docs/vision/governed-tools.md)
- [Image privacy](../docs/official_docs/security/image-privacy.md)
- [Cloud vision backends](../docs/official_docs/vision/cloud.md)
- [Spec 3.7](../docs/design/spec-3.7-vision.md)

## License

Apache-2.0
