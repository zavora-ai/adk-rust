# Restricting What Leaves the Machine

An agent with `adk-vision` reads, detects and describes images out of the box: with no
`.adk/vision.toml`, an image a remote backend is called with leaves as the agent sends it,
unredacted and unasked, to a public host or to a model on a private network, and its release
is still recorded. Every restriction below is an opt-in. Once chosen, it is enforced on every path the framework controls — the vision tools
release images only through the egress gate, and `ImagePrivacyGuardrail` runs the same gate
on model requests, tool results and user input — and it fails closed: an image it holds,
refuses or cannot redact never leaves (spec 3.7 §7.8, §9, §13 Q1 revised 2026-11-25).

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["guardrail"] }
```

## The Opt-Ins

| Restriction | Set with | What it does |
|-------------|----------|--------------|
| Ask | `default = "ask"`, or a rule with `action = "ask"` | Holds the release until a person approves the exact redacted image, by its egress digest |
| Deny | `default = "deny"`, or a rule with `action = "deny"` | Refuses the release and records why |
| Per destination | `[[egress.rule]]` with `host` (`*.example.com` for subdomains) or `provider` (`anthropic`, `gemini`, `bedrock`, `nvidia`) | Applies the rule's action to that host, or to every host of that provider |
| Redact | `redact = ["faces", "personal_text"]` | Covers each listed class on the device before the decision; a listed class with no redactor refuses the release |
| On device | `on_device = true` | Refuses every release at the destination step |
| Public networks only | `private_networks = false`, or the gate built with `EgressPolicy::public_only()` or an allowlist | Refuses a host on a loopback, private or link-local address at the destination step; the gate's policy is the ceiling the switch narrows. Cloud metadata endpoints are refused either way |
| Stream privacy | `with_privacy(hook)` on `StreamPipeline::builder`, `SourceSpec` or `LiveCamera` | Redacts every frame before any other stage sees it |
| Cover people on the live camera | `LiveCamera::cover_people` | Covers every detected person before a frame is shown or kept |
| Distrust the workspace file | `VisionPack::distrust_workspace_rules` | A workspace `.adk/vision.toml`'s `allow` rules and default ask, and with no file every release asks |
| Harness mode | ReadOnly or Plan | Denies every vision call that sends an image off the machine, whatever the rules allow |

```toml
# .adk/vision.toml: crops to Anthropic go after faces and personal text are covered; whole
# pages ask; nothing goes to OpenRouter.
[egress]
default = "ask"
redact = ["faces", "personal_text"]

[[egress.rule]]
provider = "anthropic"
classes = ["document_crop"]
action = "allow"

[[egress.rule]]
provider = "openrouter"
action = "deny"
```

A rule names exactly one of `host` and `provider`; rules match in order, the first match
decides, and a backend with several hosts gets the strictest of their decisions. A
`provider` is the id a model's responses carry in `LlmResponse::provider`, which
`adk_core::Llm::data_destinations` names with the hosts of the client's configured endpoint:
a custom base URL, a Vertex AI location or a Bedrock region is the host a rule sees.

## What Is Redacted

When the rules list a class, the gate fills each region solid black — blur and pixelation
of text can be reversed — then encodes the copy that leaves. Redactors run on the device and
answer in the pixels of the image about to be released:

| Class | Redactor | Finds |
|-------|----------|-------|
| `faces` | `FaceRedactor` over an on-device face `Detector`, with a person `Detector` as its fallback | Each detected `face`, widened by `FACE_MARGIN` (15% of its longer side), and the whole box of each detected `person` with no face inside (spec 3.7 Q3) |
| `faces` | `PersonRedactor` over any on-device `Detector` | The whole box of every detected `person`, for a deployment with no face detector |
| `personal_text` | `PersonalTextRedactor` over any on-device `DocumentReader` | Words `adk_guardrail::PiiRedactor`'s patterns match — email, phone, SSN, card number — and the values of labelled fields such as `Name:` |
| `marked` | The caller's regions | Regions a call names for redaction |

`PiiRedactor::find` reports each match as a byte range of the text it searched;
`PersonalTextRedactor` joins each block's words with single spaces, so every range maps to
the words it covers and their boxes. A labelled field's value is covered whether the reader
returns it on the label's line or as the next block to its right. A class the rules list
with no redactor installed refuses every release, and a redactor refuses a remote backend.

A face counts as a person's when its centre lies inside the person's box, so a person seen
from behind, or a face turned away or too small for the face detector, is still covered —
as the whole person. A person whose face is found keeps the rest of the picture: a close
desk frame is no longer blanked by one person's box. An approval card draws a face as
`faces` and a person covered whole as `people`, and its caption says which:

| Covered | Caption |
|---------|---------|
| One face | `1 face covered` |
| One person, no face found | `1 face · the whole person is covered` |
| Both | `2 faces covered, 1 as the whole person` |

The receipt and the `vision.egress.released` record count both under `faces`, the class the
rules list.

### The Default Redactors

Each redactor takes any on-device backend, or builds the default one from `adk-vision`'s
built-in manifest:

| Constructor | Feature | Backend | Weights |
|-------------|---------|---------|---------|
| `FaceRedactor::on_device(fetcher)` | `onnx` | YuNet faces at a score of 0.5 or more; RF-DETR Nano for each `person` at 0.25 or more with no face found | `yunet-2023mar`, 232 KB, MIT; `rf-detr-n`, 108 MB, Apache-2.0 |
| `PersonRedactor::rf_detr(fetcher)` | `onnx` | RF-DETR Nano, COCO `person` at a score of 0.25 or more | `rf-detr-n`, 108 MB, Apache-2.0 |
| `PersonalTextRedactor::pp_ocr(fetcher, size)` | `guardrail`, `ocr` | PP-OCRv6 tiny, small or medium | `pp-ocrv6-{size}-det` and `-rec`; small is 31 MB, Apache-2.0 |

The default backend loads on the redactor's first use, through the `ModelFetcher`, with every
file checked against its SHA-256 before it loads. A load that fails — `HF_HUB_OFFLINE` with the
weights not cached, a digest mismatch, no network — fails that call, so the gate refuses the
release at its redact step and records why; the next call tries the load again. One load
errs toward covering instead: a `FaceRedactor` whose face detector cannot load covers every
person its fallback finds as the whole person, with a warning. The harness vision pack
installs these defaults (feature `vision-local`) for `faces` and `personal_text` when the
rules list them and no redactor of the operator's covers them; `PERSONAL_FIELD_LABELS` lists
the field labels it covers.

> **Note:** RF-DETR is trained on photographs: the drawn portrait of the test identity card
> scores 0.11 as a `person`. YuNet finds the drawn faces of the scenario fixtures (scores 0.64
> to 0.91 from 60 px to most of a 1280×720 frame), so `FaceRedactor` covers them too.

### The Face Detector

Spec 3.7 Q3 asked for a permissively licensed face detector with an ONNX export. The
candidates, with the licence of their weights:

| Detector | Code | Weights | ONNX | Decision |
|----------|------|---------|------|----------|
| **YuNet 2023mar** (OpenCV Zoo, `opencv/face_detection_yunet`) | MIT | MIT ("all files in this directory", the model included) | Official, 232 KB, opset 11 | **Chosen**: `yunet-2023mar` in the built-in manifest |
| SCRFD, RetinaFace (InsightFace) | MIT | Non-commercial research only (InsightFace README) | Official | Excluded |
| YOLOv5-face, YOLOv8-face | GPL-3.0, AGPL-3.0 | Same as the code | Community | Excluded (copyleft) |
| BlazeFace (MediaPipe) | Apache-2.0 | Apache-2.0 (Google model card) | Community conversions only, 128×128, about 2 m range | Not chosen: no first-party ONNX, short range |
| UltraFace (Linzaer) | MIT | MIT | ONNX Model Zoo, 320×240 | Not chosen: lower accuracy than YuNet |

YuNet and most face detectors are trained on WIDER FACE, whose images are distributed for
research; the published YuNet weights carry the MIT licence regardless, as OpenCV itself
ships them. A deployment that needs a detector trained on other data registers its own
manifest and passes the detector to `FaceRedactor::new`.

Face detection redacts and nothing else (V-I8): YuNet detects faces, it never recognises or
compares them, and no manifest holds a recognition model.

The `adk-harness` test `s3_the_default_redactors_leave_no_face_and_no_personal_string` checks
the harness vision pack's default redactors on the identity-card fixture and two COCO
photographs, under rules that list both classes: YuNet finds no face, and PP-OCRv6 tiny,
small and medium none of the seeded personal strings, in the bytes the provider received.

## Faces on Streams

A stream's frames are not redacted unless a privacy hook is given: `with_privacy(hook)` on
`StreamPipeline::builder`, `SourceSpec` or `LiveCamera`, and `keyframe_privacy(hook)` on the
video analyst. A hook runs before any other stage, so a stored frame was redacted before its
write (V-I9). `RedactorPrivacy::blur_faces(fetcher)` (feature `onnx`) is the face blur of
spec 3.7 S18: `FaceRedactor::on_device` under `RedactionMethod::FACE_BLUR`, a Gaussian whose
standard deviation is a quarter of each box's longer side, so a near face is blurred as hard
as a far one, with each face held for two frames so a frame in which YuNet misses a moving
face still blurs it. See [Streams](../vision/streams.md#the-privacy-step).

## The On-Device Profile

`on_device = true` in the `[egress]` table of `.adk/vision.toml` keeps every image on the
machine: the gate refuses every release at its destination step, before anything is cropped
or redacted, whatever the rules say, and records the refusal with its reason. An escalation
it refuses leaves the on-device answer standing. The harness vision pack's default fetcher
then works offline, as under `HF_HUB_OFFLINE=1`, and so does `adk-rust vision run`.

```toml
# .adk/vision.toml
[egress]
on_device = true
```

Run the agent with `HF_HUB_OFFLINE=1` and a warm cache, and inside a sandbox with the network
denied — Seatbelt on macOS, bubblewrap on Linux ([Code Execution Sandbox](../sandbox/index.md)):

| Layer | What it refuses | Where it is recorded |
|-------|-----------------|----------------------|
| The egress gate | Every image release, at the destination step | `vision.egress.refused`, the `EgressRecorder` |
| The model fetcher | Every download: a cached file is verified and returned, a missing one is an error | The error |
| The sandbox | Every socket the process opens | An `adk-sandbox` egress proxy that admits no host records each attempt |

Scenario S19 of `examples/vision` runs S1 this way and checks that the forty pages are read
on the device, that each of the five escalations is refused at the gate with its reason, and
that the sandbox's egress record is empty. On Windows the sandbox does not enforce a denied
network, so only the gate and the fetcher hold there.

## The Guardrail

| Path | Hook | Behaviour |
|------|------|-----------|
| Model calls | `ImagePrivacyGuardrail::before_model_callback`, on an agent whose model is remote | Each image part in the request — user uploads and the inline images of function responses — is released as data class `model_context`, or replaced by a text placeholder naming its digest and why it was withheld |
| Tool results | `ToolGuardrail::validate_result` | A base64 image in a result's JSON — a `data:image/...;base64,` URL or bare PNG, JPEG, WebP or GIF base64 — is replaced by its released copy or a placeholder |
| User input | `Guardrail::validate` | As for model calls, returned as `Transform` |

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_vision::{EgressGate, ImagePrivacyGuardrail};

fn guarded(
    model: Arc<dyn adk_core::Llm>,
    gate: Arc<EgressGate>,
) -> adk_core::Result<adk_agent::LlmAgent> {
    // No hosts given: the guardrail releases to the hosts the model names.
    let guardrail =
        Arc::new(ImagePrivacyGuardrail::for_model(gate, model.as_ref(), Vec::<String>::new()));
    LlmAgentBuilder::new("desk")
        .model(model)
        .before_model_callback(guardrail.before_model_callback())
        .build()
}
```

- **Locality.** `ImagePrivacyGuardrail::for_model` reads `Llm::data_locality()`. A model in
  this process or on loopback is not guarded, since nothing leaves.
- **Destinations.** Images are released to the hosts given, or with none to the hosts and
  providers of `Llm::data_destinations()`.
- **Released parts.** A part the gate released carries a release annotation; it passes again
  only when its bytes hash to a digest that gate released, so a forged annotation is gated.
- **Rules.** Under the default `allow`, an image bound for a model's context is released as
  it is. A rule that asks or denies for the model's host or provider and class
  `model_context` withholds it with a placeholder; the vision tools can still read it on the
  device.
- **Boundary.** User code that calls an `Llm` directly, without the guardrail, is outside this
  guarantee.

## Safeguards That Always Apply

These hold whatever the rules say; the configurable ones are set where the table names.

| Safeguard | Default | Configure with |
|-----------|---------|----------------|
| Decode caps on every image | 50 MB and 50 megapixels | `DecodeLimits::with_max_bytes`, `DecodeLimits::with_max_pixels` |
| Per-backend input limits | Each adapter's documented limit; larger images downscaled, with the resize on the transform chain | `LlmDescriber::with_limits`, `BackendDescriptor::with_limits` |
| Model digest and size checks | Every file checked against its manifest's SHA-256 and size before it loads | A user manifest in a `ManifestSet` names its own files |
| Licence acceptance for use-restricted weights | Nothing loads until a person records acceptance | `adk-rust vision models accept`, or an `AcceptanceStore` |
| Text read from an image is untrusted | Screened and delivered inside a delimited block in the harness | — |
| Screen-plan limits | A plan holds at most 10 actions | `ComputerUsePack::with_plan_max_actions`, `ScreenPlan::with_max_actions` |
| Unknown keys in `.adk/vision.toml` | Refused, not ignored | — |
| Every release recorded | Before the request is sent | An `EgressRecorder`, the run's audit trail |

## Invariants

| | Invariant | Enforced by |
|---|-----------|-------------|
| V-I1 | A remote backend receives only a `ReleasedImage`, and only the egress gate constructs one | The type system: remote backends implement their trait for `ReleasedImage` |
| V-I2 | Redaction of every listed class precedes the decision and the release; no configuration releases before it | The gate's step order; `.adk/vision.toml` refuses unknown keys |
| V-I3 | Every release writes its record, with source and egress digests and the destination, before the request is sent | `EgressRecorder` and the run's audit trail (`vision.egress.released`) |
| V-I4 | With the vision pack, the guardrail is on for remote model calls, and no agent tool removes it | `adk-harness` feature `vision` |
| V-I5 | A held vision call carries its evidence, and a different image holds it again | The egress digest in the call's fingerprint |
| V-I6 | A priced call opens its hold before its request | The runtime's preflight, before any image is released |
| V-I8 | Face and person detection only redact; no manifest holds a recognition model | `FaceRedactor`, `PersonRedactor`; the built-in manifest |
| V-I9 | A stored frame passed its source's privacy hook, when one is set, before the write | `FramePrivacy` makes the only frame a `FrameKeeper` stores |
| V-I15 | Records carry digests and artifact references, never pixels | `vision.*` records |
| V-I17 | A freeze refuses every vision call and every release | `VisionCx::check` at each step |

## Related

- [Governed Vision Tools](../vision/governed-tools.md)
- [Guardrails](guardrails.md)
- [Network Egress Policy](egress.md)
- [Tool Authorization](tool-authorization.md)
