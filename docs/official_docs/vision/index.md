# Vision

The `adk-vision` crate gives agents local-first perception: detection, segmentation,
document reading, embedding, description, tracking and editing behind one set of
contracts, with every region in source pixels, every weight file verified, and every image
that leaves the machine cropped, redacted and recorded first. It implements
[spec 3.7](../../design/spec-3.7-vision.md).

> **Note:** `adk-vision` is **Experimental**. This release holds the contracts, the runtime,
> the egress boundary, the conformance suite, the [local ONNX backends](onnx.md) and the
> [cloud backends](cloud.md) — Gemini, Claude, Muse and NVIDIA NIM describers and Meta
> `sam-3.1`. OCR and mistral.rs backends arrive as features in later releases.

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["fetch"] }
```

## Concepts

| Concept | Type | What it is |
|---------|------|------------|
| Source image | `SourceImage`, `ImageRef` | Pixels decoded once on the device, named by the `sha256:` digest of their encoded bytes; `MediaStore` keeps them as artifacts |
| Sent image | `SentImage` | What an on-device backend sees: pixels and the transform chain from the source. Only `VisionRuntime` makes one |
| Released image | `ReleasedImage` | What a remote backend sees: the cropped, redacted copy, its bytes and its egress receipt. Only `EgressGate` makes one |
| Raw region | `RawRegion` | A backend's answer, in the backend's own coordinate frame. Never leaves the call that produced it |
| Region | `Region`, `RegionSet` | An answer in source pixels, with its backend, model, frame, transform chain and any escalation |
| Manifest | `ModelManifest` | A model's files pinned by revision and SHA-256, its licence, runtime and input |
| Verified model | `VerifiedModel` | A model whose every file matched its manifest under a licence the gate allowed |

### The Traits

| Trait | Does | Returns |
|-------|------|---------|
| `Detector` | Finds objects for closed-set classes or open-vocabulary concepts | Boxes and masks |
| `Segmenter` | Segments for concept, point, box or mask prompts | Masks and polygons |
| `DocumentReader` | Reads a page: layout, text, tables | Blocks with regions, words, cells and reading order |
| `Embedder` | Embeds images and texts into one `EmbeddingSpace` | Vectors |
| `Describer` | Answers in text, a schema-shaped value, and regions when asked | `Description` |
| `Tracker` | Associates detections across frames, pure CPU | Regions with anonymous `TrackId`s |
| `VideoAnalyzer` | Tracks, captions or answers over sampled frames | `VideoAnalysis` |
| `Editor` | Crops, resizes, redacts, blurs, pixelates, annotates | A new image with `Provenance::Derived` |

Each image-taking trait is generic over what its backend is handed. An on-device backend
implements `Detector` (that is, `Detector<SentImage>`); a remote backend implements
`Detector<ReleasedImage>`, and handing it a `SentImage` does not compile. The built-in
`DeterministicEditor` and `GreedyIouTracker` need no model.

## Coordinate Frames

Every backend descriptor names the frame it answers in:

| `CoordFrame` | Providers | Meaning |
|--------------|-----------|---------|
| `SentPixels` | Meta `sam-3.1`; Claude, once the runtime has resized the image to Claude's limit; local backends that undo their own letterbox | Pixels of the image as sent |
| `Normalized { scale, order }` | Gemini `box_2d`: `scale: 1000`, `order: Yx` | Numbers on a fixed scale, in the provider's axis order |
| `ModelInput { width, height, letterbox }` | RF-DETR, SAM 2 | Pixels of the model's input tensor, with the sent image letterboxed into it |

Between the source image and the sent image, the runtime records a `TransformChain` of
`Crop`, `Resize`, `Pad`, `Rotate` and `Redact` steps. Mapping a raw region to source pixels
runs the frame, then the chain in reverse, then clamps to the image:

```rust
use adk_vision::{
    AxisOrder, BackendId, CoordFrame, Digest, RawRegion, RegionNormalizer, Transform,
    TransformChain,
};

// A 3024x1964 screenshot, cropped to a field and its margin, sent to Gemini.
let chain = TransformChain::new(3024, 1964)
    .then(Transform::Crop { x: 1900, y: 1160, width: 220, height: 120 })
    .unwrap();
let gemini = CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx };
let normalizer = RegionNormalizer::new(Digest::of(b"screenshot"), chain, gemini, BackendId::new("gemini"));

// Gemini answers [ymin, xmin, ymax, xmax] on 0-1000 of the crop.
let raw = RawRegion::boxed(gemini.box_from_raw([167.0, 91.0, 833.0, 909.0]), "save", 0.9);
let region = normalizer.region(raw).unwrap().unwrap();
let b = region.shape.bbox().unwrap();
assert!((b.x0 - 1920.0).abs() < 1.0 && (b.y0 - 1180.0).abs() < 1.0);
```

The crate's tests hold Gemini, Claude, `sam-3.1` and RF-DETR answers for twelve targets
within 2 px of the truth after normalisation, and random transform chains within 0.5 px.

### Screen Points

`ScreenSpace` turns screenshot pixels into the logical points a click uses. A screenshot of
a 1512×982-point display at device pixel ratio 2 is 3024×1964 pixels:

```rust
use adk_vision::ScreenSpace;

let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
assert_eq!(screen.to_points([2010.0, 1220.0]), [1005.0, 610.0]);
```

### The Wire Form

`RegionSet` — the image's digest and dimensions plus its regions — is published as a JSON
Schema in `adk-vision/schema/region-set.json`. Shapes carry a `kind` tag, transforms an
`op` tag, and fields are camelCase:

```json
{
  "image": "sha256:…",
  "width": 3024,
  "height": 1964,
  "regions": [{
    "image": "sha256:…",
    "shape": { "kind": "box", "x0": 1920, "y0": 1180, "x1": 2100, "y1": 1260 },
    "label": "save",
    "score": 0.9,
    "origin": {
      "backend": "gemini",
      "frame": { "kind": "normalized", "scale": 1000, "order": "yx" },
      "chain": {
        "sourceWidth": 3024,
        "sourceHeight": 1964,
        "steps": [{ "op": "crop", "x": 1900, "y": 1160, "width": 220, "height": 120 }]
      }
    }
  }]
}
```

## The Runtime

`VisionRuntime` routes each call:

| Step | What happens |
|------|--------------|
| Route | The call's route, else the task's, else the first registered on-device backend; registration order decides, no backend is privileged |
| Prepare | An on-device backend gets a `SentImage` downscaled to its `InputLimits` (or a refusal naming the limit); a remote one gets a `ReleasedImage` from the egress gate |
| Meter | The budget hold opens before the image is prepared or released, so a call the budget cannot cover sends nothing and leaves no egress record; images (and pages, for reading) are committed as units, at the unit price for a per-unit backend and zero for a local one; a token-billed backend's `TokenEstimate` sizes its hold |
| Cancel | The `VisionCx` cancellation token ends the call at once; a frozen kill switch refuses it before the backend runs |
| Normalise | Every answer is mapped to source pixels |
| Escalate | On a trigger, the uncertain region's crop goes to the escalation backend, whose answer replaces the primary's with `escalatedFrom` |

| `Trigger` | Escalates | Sends |
|-----------|-----------|-------|
| `LowConfidence { threshold }` | Each region, block or description scored below the threshold | The region's crop plus margin |
| `NoResult` | A call that found nothing | The whole image: no region exists to crop |
| `Disagreement { with, min_iou }` | Regions a second on-device backend does not confirm (detection and segmentation) | Each region's crop plus margin |

Each call returns its `CallRecord`s — backend, locality, image and egress digests, units,
cost, region count, duration — the fields of the `vision.call` audit record.

## The Egress Gate

`EgressGate::release` is the only way an image reaches a remote backend:

1. **Destination.** The adk-core `EgressPolicy` decides each of the backend's hosts.
2. **Minimise.** The image is cropped to the request's crop and downscaled to the
   backend's limit.
3. **Redact.** Each class the configuration requires is found by its `Redactor` and filled
   with solid colour, because blur and pixelation of text can be reversed; then the
   caller's marked regions. A required class with no redactor refuses the release.
4. **Decide.** The first `[[egress.rule]]` matching the host and data class, else the
   default, says `deny`, `ask` or `allow`. `ask` holds the release until a person approves
   its exact egress digest; a different image is a different digest and is held again.
5. **Record.** With a media store (`EgressGate::with_media_store`), the egress copy is
   stored as an artifact. The `EgressReceipt` goes to the `EgressRecorder`, and a
   `vision.egress.released` record to the call's audit trail, before the image is
   returned; a refusal is recorded too.
6. **Release.**

`.adk/vision.toml` holds the rules. There is no setting that releases before redaction,
and an unknown key is an error:

```toml
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
host = "generativelanguage.googleapis.com"
classes = ["document_crop"]
action = "ask"
```

## Model Manifests and Licences

```json
{
  "id": "rf-detr-n-q4f16",
  "task": "detect",
  "source": {
    "kind": "huggingFace",
    "repo": "onnx-community/rfdetr_nano-ONNX",
    "revision": "eae21cee0687a91bcf9fa071605c48d7705d2d91"
  },
  "files": [{
    "path": "onnx/model_q4f16.onnx",
    "sha256": "sha256:42d8072bc00975405531e4e7f20dda40a6cb5ed67e345ecd764f92748463a996",
    "size": 18996625
  }],
  "licence": {
    "id": "Apache-2.0",
    "url": "https://huggingface.co/onnx-community/rfdetr_nano-ONNX",
    "class": "permissive"
  },
  "runtime": { "kind": "ort", "opset": 17 }
}
```

A revision is a 40-digit commit hash, never a branch. `ManifestSet::builtin()` holds the
built-in entries, which arrive with each backend, pinned by the benchmark run that accepts
the model; `with_dir` adds the user's own manifests, such as an RF-DETR fine-tune.

| `LicenceClass` | Fetched and loaded when |
|----------------|-------------------------|
| `permissive` | Always |
| `useRestricted` | A person's acceptance of that licence URL is recorded; `adk-rust vision models accept <id>` shows the licence first, and no agent tool records one |
| `nonCommercial`, `copyleft` | The user's `LicencePolicy` names the model; never in the built-in manifest |

### Fetching

`ModelFetcher` (feature `fetch`) downloads through `hf-hub` 0.5 into the shared Hugging Face
cache layout:

| Variable | Effect |
|----------|--------|
| `HF_HUB_CACHE` | The cache directory |
| `HF_HOME` | The cache is `$HF_HOME/hub` when `HF_HUB_CACHE` is unset; otherwise `~/.cache/huggingface/hub` |
| `HF_TOKEN` | The access token for gated repositories |
| `HF_ENDPOINT` | A mirror instead of `https://huggingface.co` |
| `HF_HUB_OFFLINE=1` | No socket is opened: a cached file is verified and returned, a missing one is an error |

The licence gate runs before any request. Each file's size and SHA-256 are checked after
download and before every load, and a mismatch is refused with both digests. A 401 or 403
from the hub is a gated repository and becomes `LicenceAcceptanceRequired` naming the
repository page. Backends take a `VerifiedModel`, which only verification constructs.

```rust,no_run
use std::sync::Arc;
use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ModelFetcher, ModelManifest};

# async fn demo(manifest: ModelManifest) -> adk_vision::Result<()> {
let fetcher = ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
let model = fetcher.fetch(&manifest).await?;
let file = model.file("onnx/model_q4f16.onnx").unwrap();
println!("{} {}", file.path().display(), file.digest());
# Ok(())
# }
```

## Writing a Backend

1. Describe it: a `BackendDescriptor` with its id, `Locality`, `CoordFrame`, price and
   `InputLimits`.
2. Implement the trait for `SentImage` if it runs on the device, or for `ReleasedImage` if
   it sends the image off the machine. Answer with `RawRegion`s in your frame.
3. Register it: `VisionRuntimeBuilder::detector` or `remote_detector`, and so on; a
   descriptor whose locality does not match is refused.
4. Run the conformance suite with feature `conformance`:

```rust,ignore
mod my_detector {
    use std::sync::Arc;
    use adk_vision::conformance::DetectorTarget;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Local(Arc::new(my_crate::MyDetector::new())),
    }
}
```

| Case | Checks |
|------|--------|
| Coordinates | Regions within the fixture's tolerance in source pixels, inside the image, with backend, frame and chain |
| Usage | Images recorded as units; zero cost for a local backend, the unit price for a per-unit one |
| Cancellation | A cancelled call ends with `Cancelled`; a slow backend within one call |
| Kill switch | A frozen control refuses before the backend runs and nothing is released |
| Egress | A local backend leaves the egress recorder empty; every remote image is a recorded release |
| Unreleased | A remote backend without an egress gate is refused as `Forbidden` |
| Oversized | Refused with the limit in the message, or downscaled with the resize recorded |
| Errors | A rate limit maps to `RateLimited` with a retry hint |

`EgressGate::stage` runs the first three steps and stores the copy without releasing it, so
an approval card can show the exact image; see [Governed Vision Tools](governed-tools.md).

## Related

- [Governed Vision Tools](governed-tools.md)
- [Image Privacy](../security/image-privacy.md)
- [Local ONNX backends](onnx.md)
- [Cloud vision backends](cloud.md): `LlmDescriber`, NVIDIA NIM and Meta `sam-3.1`
- [Egress control](../security/egress.md)
- [Tool authorization](../security/tool-authorization.md)
- [Artifacts](../artifacts/artifacts.md)
