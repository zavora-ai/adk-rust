# adk-vision

Local-first perception for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-vision.svg)](https://crates.io/crates/adk-vision)
[![Documentation](https://docs.rs/adk-vision/badge.svg)](https://docs.rs/adk-vision)
[![License](https://img.shields.io/crates/l/adk-vision.svg)](../LICENSE)

> **Note:** `adk-vision` is **Experimental** (spec 3.7). This release holds the contracts —
> media, regions, manifests, traits, the runtime, the egress boundary and the conformance
> suite, the local ONNX backends (`onnx`), the document readers (`ocr`, `ocr-vl`, `nim`), and
> the cloud backends: Gemini, Claude, Muse and NVIDIA NIM describers (`cloud`, `nim`) and Meta
> `sam-3.1` (`sam`). mistral.rs backends arrive as a feature in a later release.

## Overview

`adk-vision` gives an agent sight through contracts first:

| Contract | Type | What it guarantees |
|----------|------|--------------------|
| Media | `ImageRef`, `Frame`, `VideoRef`, `MediaStore` | Pixels enter once and are stored as artifacts by `sha256:` digest; the digest is what travels |
| Regions | `Region`, `RegionSet`, `CoordFrame`, `TransformChain` | Every region is in source-image pixels and records the frame its backend answered in and the transforms in between |
| Manifests | `ModelManifest`, `LicenceGate`, `VerifiedModel` | Every weight file is pinned by revision and SHA-256 and names its licence; nothing loads unverified |
| Traits | `Detector`, `Segmenter`, `DocumentReader`, `Embedder`, `Describer`, `Tracker`, `VideoAnalyzer`, `Editor` | One contract per perception task, each with a conformance suite |
| Runtime | `VisionRuntime` | Local-first routing, metering, cancellation, mapping to source pixels, crop escalation |
| Egress | `EgressGate`, `ReleasedImage`, `ReleasedClip` | A remote backend only ever receives an image the gate cropped, redacted, approved and recorded, or a clip it checked and recorded |
| Video | `SourceClip`, `VideoSource`, `FramesAnalyzer`, `VisibilityPass`, `SamVideoTracker` | `analyze_video` holds a call's frames before anything leaves; questions over sampled frames on any model (`cloud`), optionally only around the first frame that shows the question's subject; tracks through a clip with `sam-3.1` video (`sam`) |
| Tools | `VisionToolset` | `vision_detect`, `vision_segment`, `vision_read`, `vision_describe`, `vision_crop`, `vision_redact`, `vision_annotate`, `vision_measure` on the governed path; a remote call is held with the exact egress image and releases only that digest |
| Watch | `WatchCondition`, `WatchEvaluator`, `WatchSink` | Conditions held on the device against tracked objects; one event per object, keyed for the kernel's `VisionWakeSource`, never carrying pixels |
| Memory | `VisualMemory`, `VisualQuery`, `SiglipTextEmbedding` | Image embeddings as `adk-rag` chunks in any vector store, found by text or an example image; one embedding space per collection; captions with artifact references in `adk-memory`; erasure by source (`memory`) |
| Measurement | `Calibration`, `Measurement` | A fiducial of known size gives pixels per millimetre; maximum length and area from masks with their uncertainty, valid only in the fiducial's plane |
| Privacy | `PersonRedactor`, `PersonalTextRedactor`, `ImagePrivacyGuardrail` | Faces and personal text filled solid black on the device — by default RF-DETR Nano (`onnx`) and PP-OCRv6 (`ocr`), loaded on first use; the same gate on model requests, tool results and user input |

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["fetch"] }
```

Through the umbrella crate, `adk-rust` feature `vision` (in `full`) brings the pure-Rust
features — `fetch`, `cloud`, `nim`, `sam`, `guardrail` — as `adk_rust::vision`, and
`vision-local` (in no tier) adds the on-device backends: `onnx`, `ocr` and `ocr-vl`.

| Feature | Contents |
|---------|----------|
| (always) | media, geometry, manifests, the traits, `VisionRuntime`, the egress gate, the deterministic `Editor`, `VisionToolset`, `PersonRedactor`, the `vision.*` audit records, watch conditions, calibration and measurement, the verifier loop (`VerifierLoop`) |
| `fetch` | `ModelFetcher`: Hugging Face downloads by revision through `hf-hub` 0.5, SHA-256 checked before load, `HF_HUB_OFFLINE` honoured |
| `conformance` | The conformance suite and its per-trait test macros |
| `eval` | `VisionEvalAgent`: any runtime task answering `adk-eval` cases in the vision tools' response form, in process or over the `cargo adk eval` protocol (`serve_eval_protocol`) |
| `guardrail` | `ImagePrivacyGuardrail` (a `BeforeModelCallback`, an `adk-guardrail` `Guardrail` and `ToolGuardrail`) and `PersonalTextRedactor` over `PiiRedactor`'s byte ranges |
| `onnx` | Local backends on ONNX Runtime: `RfDetr`, `EfficientSam`, `Sam2`, `SigLip2`; onnxruntime downloaded at build time |
| `onnx-dynamic` | The same backends against an onnxruntime library loaded from `ORT_DYLIB_PATH` |
| `onnx-coreml`, `onnx-cuda` | The CoreML and CUDA execution providers for the `onnx` backends |
| `cloud` | `LlmDescriber`: any `adk_core::Llm` that takes images as a `Describer`, with Gemini (`box_2d`, 0–1000), Claude (pixels of the image as sent) and prompt-fixed 0–1000 region adapters, and provider-formula preflight estimates |
| `nim` | NVIDIA NIM: vision-language models (`NimConfig`, reasoning off, unpriced) and `NimDocumentReader` for NVIDIA's OCR, page-element, table and chart endpoints, remote, behind the egress gate; implies `cloud` |
| `sam` | `SamSegmenter`: Meta `sam-3.1` request building, stream parser and mask placement, billed per request at $2.50 per 1,000 images; masks as boxes, or decoded by a program you installed (`ExternalMaskDecoder`) |
| `sam-codec` | `LicensedMaskDecoder`: `sam-3.1` masks decoded in process by `adk-vision-sam-codec`, the port of Meta's codec under the SAM License, once a person accepts it (`adk-rust vision models accept sam-mask-codec`); in no `adk-rust` tier; implies `sam` |
| `ocr` | `PpOcrReader`: PP-OCRv6 text lines and words through `oar-ocr` on ONNX Runtime |
| `ocr-vl` | `PaddleOcrVlReader`: PaddleOCR-VL-1.6 with PP-DocLayoutV3 through `oar-ocr-vl` on Candle |
| `ocr-vl-metal` | `ocr-vl` on Apple GPUs through Candle's Metal backend |
| `mistralrs` | `LlmDescriber::mistralrs`: Qwen3-VL (`qwen3-vl-2b-instruct`, `qwen3-vl-4b-instruct` and `qwen3-vl-8b-instruct`, built in) or another mistral.rs vision model in this process, verified before load, regions in Qwen3-VL's own 0–1000 frame; implies `cloud`, `fetch` |
| `mistralrs-metal`, `mistralrs-cuda` | `mistralrs` on Apple GPUs or CUDA |
| `stream` | `FrameSource`, `Sampler`, the `FrameGovernor`, `FrameKeeper` retention and `StreamPipeline` (source, sampler, governor, privacy hook, detector, tracker, observation sink) |
| `rtsp` | `RtspSource`: RTSP cameras through `retina`; MJPEG decoded in process, H.264 and H.265 with `ffmpeg` |
| `ffmpeg` | `FileSource` (MP4, MOV) and H.264/H.265 decoding through a user-installed `ffmpeg`; nothing vendored or downloaded |
| `device` | `DeviceSource`: a camera on this machine (AVFoundation on macOS, v4l2 on Linux, DirectShow on Windows) through the same user-installed `ffmpeg`, `DeviceSource::list` and a `testsrc2` stand-in; implies `ffmpeg` |
| `screen`, `browser`, `surface-camera` | `ScreenSource` (a `ComputerActuator` or computer-use-mcp observations), `BrowserSource` (`browser_screenshot`), `SurfaceCameraSource` (frames the web home pushes) |
| `stream-fixtures` | The generated conveyor clip and an in-process RTSP test server |
| `track` | `ByteTracker`: ByteTrack through `trackforge` 0.5, anonymous tracks that keep their identifiers through jitter and occlusion |
| `computer-use` | `GroundedTarget::target_evidence`: a chosen set-of-marks candidate as `adk-computer-use` `TargetEvidence`, bounds in logical points at the screenshot's device pixel ratio |
| `gemini-video` | `GeminiVideoAnalyzer`: Gemini native video, uploading clips above 20 MB once through the File API, at an explicit frame rate and media resolution; implies `cloud` |
| `memory` | `VisualMemory`: image embeddings as `adk-rag` chunks with the caption, artifact, digest, region, source, capture time and run; text and example-image search with day, kind and source filters; the collection's embedding space recorded and any other refused; captions in `adk-memory` and sources in its graph memory; `erase_source` with a `vision.memory.erased` record; the S16 week corpus |
| `memory-lancedb` | The visual memory tests against `adk-rag`'s embedded LanceDB backend (needs `protoc`) |
| `team` | `team`: the `Perceiver`, `Reasoner` and `Verifier` roles, each refusing at construction what it may not hold, and `VisionTeam`, which escalates a reasoner/verifier disagreement to a person on one card with both opinions and the frame; implies `guardrail` |

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

`adk_vision::screen` grounds actions on a screenshot by set-of-marks: `MarkSet` numbers the
candidates from `browser_elements`, local OCR words or a model's point, `vision_annotate`
draws them at the device pixel ratio, and the chosen mark's `GroundedTarget` is a
`browser_click` selector or a click point in logical points with the screenshot's digest.
`FittedActuator` resizes screenshots to Claude's image limits for `computer_toolset_20260801`
and scales clicks and `zoom` regions back; `ScreenPlan` is one approval for a previewed plan of
up to N actions in one window, each action checked against its preview before it runs. See
[Screen Grounding](../docs/official_docs/vision/screen.md).

## Remote Backends and the Egress Gate

An on-device backend implements its trait for `SentImage`; a remote backend implements it
for `ReleasedImage`, so handing a remote backend an unreleased image does not compile. Only
`EgressGate::release` constructs a `ReleasedImage`, after six steps:

1. **Destination** — the adk-core `EgressPolicy` decides each host.
2. **Minimise** — crop to the region the call needs, downscale to the backend's limit.
3. **Redact** — every class in the policy (`faces`, `personal_text`, ...) is filled with
   solid colour by its `Redactor`; a class with no redactor, or a redactor whose default
   weights cannot load (`PersonRedactor::rf_detr`, `PersonalTextRedactor::pp_ocr`), refuses
   the release.
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

## Document Readers

`PpOcrReader` (`ocr`), `PaddleOcrVlReader` (`ocr-vl`) and `NimDocumentReader` (`nim`) answer
`VisionRuntime::read` in one `DocumentPage`, published as a JSON Schema in
[`schema/document-page.json`](schema/document-page.json): blocks with regions in source
pixels, text, confidence, words and table cells, the reading order, and Markdown. The local
readers load models from the built-in manifest (`pp-ocrv6-{tiny,small,medium}-{det,rec}`,
`paddleocr-vl-1.6`, `pp-doclayoutv3`, all Apache-2.0); the NIM reader takes only released
images. See [Document readers](../docs/official_docs/vision/document-readers.md).

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
input, and rate-limit mapping; a video analyzer runs `video_analyzer_conformance_tests!`
(frame metering, egress, event times, refusals). Enable `conformance` in
`[dev-dependencies]`:

```rust,ignore
mod my_detector {
    use std::sync::Arc;
    use adk_vision::conformance::DetectorTarget;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Local(Arc::new(my_crate::MyDetector::new())),
    }
}
```

## Streams

`StreamPipeline` reads a camera, a file, a screen, a browser page or the web home's camera,
holds it to a `FrameBudget` (lower the rate, stop escalating, pause, resume next period),
redacts every frame through a required `PrivacyHook` before any other stage, detects and
tracks on the device, and hands each frame to an `ObservationSink`. Frames stay in memory
unless a wake, a card or a person keeps them through `FrameKeeper`, which stores only
redacted frames, for 30 days by default. See
[Streams](../docs/official_docs/vision/streams.md).

## Visual Memory

`VisualMemory` (feature `memory`) embeds each image through the runtime and stores it as an
`adk-rag` chunk in any `VectorStore`: the caption is its text, the artifact its `source_uri`,
and its metadata names the digest, region, source, capture time and the run that stored it.
`search_text` and `search_image` find images in the same SigLIP 2 space, and every hit links
to its artifact and run. The collection records its embedding space and refuses any other, so
a new embedder means a new collection. Captions go to `adk-memory` with a reference to their
artifact, and `erase_source` removes a source's chunks, captions, graph entity and unshared
artifacts. See [Visual memory](../docs/official_docs/vision/memory.md).

## Documentation

- [Getting started: a Document desk](../docs/official_docs/vision/getting-started.md), from
  `cargo adk new <name> --template vision`
- [The `adk-rust vision` CLI](../docs/official_docs/vision/cli.md): manifests, licences, one
  call through the egress gate, the egress log
- [Scenarios S1–S8](../examples/vision): the Document desk on local backends and recorded
  cloud responses
- [Vision overview](../docs/official_docs/vision/index.md)
- [Governed vision tools](../docs/official_docs/vision/governed-tools.md)
- [Image privacy](../docs/official_docs/security/image-privacy.md)
- [Cloud vision backends](../docs/official_docs/vision/cloud.md)
- [Vision evals](../docs/official_docs/vision/evals.md)
- [Vision benchmarks](../docs/official_docs/vision/benchmarks.md)
- [Streams](../docs/official_docs/vision/streams.md): sources, sampling, the frame governor,
  the privacy step, retention and the stream pipeline
- [Visual memory](../docs/official_docs/vision/memory.md): image embeddings as `adk-rag`
  chunks, text and example-image queries, one embedding space per collection, captions in
  `adk-memory`, erasure by source
- [Spec 3.7](../docs/design/spec-3.7-vision.md)

## License

Apache-2.0
