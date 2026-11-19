# Image Privacy

An image an agent handles can carry faces and personal details a text guardrail never sees.
`adk-vision` guards every path the framework controls by which an image could reach a remote
model: the vision tools release images only through the egress gate, and
`ImagePrivacyGuardrail` runs the same gate on model requests, tool results and user input.
An image leaves only as a cropped, redacted copy that the rules allow or a person approved,
and every release is recorded before the request is sent (spec 3.7 §7.8, §9).

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["guardrail"] }
```

## What Is Redacted

The gate fills each region solid black — blur and pixelation of text can be reversed — then
encodes the copy that leaves. Redactors run on the device and answer in the pixels of the
image about to be released:

| Class | Redactor | Finds |
|-------|----------|-------|
| `faces` | `PersonRedactor` over any on-device `Detector` | The whole box of every detected `person`, until a face detector is chosen (spec 3.7 Q3) |
| `personal_text` | `PersonalTextRedactor` over any on-device `DocumentReader` | Words `adk_guardrail::PiiRedactor`'s patterns match — email, phone, SSN, card number — and the values of labelled fields such as `Name:` |
| `marked` | The caller's regions | Regions a call names for redaction |

`PiiRedactor::find` reports each match as a byte range of the text it searched;
`PersonalTextRedactor` joins each block's words with single spaces, so every range maps to
the words it covers and their boxes. A labelled field's value is covered whether the reader
returns it on the label's line or as the next block to its right. A class the rules require
with no redactor installed refuses every release, and a redactor refuses a remote backend.

### The Default Redactors

Each redactor takes any on-device backend, or builds the default one from `adk-vision`'s
built-in manifest:

| Constructor | Feature | Backend | Weights |
|-------------|---------|---------|---------|
| `PersonRedactor::rf_detr(fetcher)` | `onnx` | RF-DETR Nano, COCO `person` at a score of 0.25 or more | `rf-detr-n`, 108 MB, Apache-2.0 |
| `PersonalTextRedactor::pp_ocr(fetcher, size)` | `guardrail`, `ocr` | PP-OCRv6 tiny, small or medium | `pp-ocrv6-{size}-det` and `-rec`; small is 31 MB, Apache-2.0 |

The default backend loads on the redactor's first use, through the `ModelFetcher`, with every
file checked against its SHA-256 before it loads. A load that fails — `HF_HUB_OFFLINE` with the
weights not cached, a digest mismatch, no network — fails that call, so the gate refuses the
release at its redact step and records why; the next call tries the load again.
`PERSONAL_FIELD_LABELS` lists the field labels the harness vision pack covers by default.

> **Note:** RF-DETR is trained on photographs. A drawn or cartoon portrait is not a `person` to
> it — the generated portrait of the test identity card scores 0.11 — so `faces` redaction
> covers people in photographs, not illustrations. Mark such regions on the call instead.

Measured on the identity-card fixture and two COCO photographs, with the harness vision pack's
default redactors and the same backends reading the bytes the provider received
(`adk-harness` test `s3_the_default_redactors_leave_no_person_and_no_personal_string`):

| Image | Redactions | People found before → after | Seeded strings read before → after (PP-OCRv6 tiny, small, medium) |
|-------|------------|-----------------------------|----------------------------------------------------------------|
| Identity card, drawn portrait | `personal_text` 6 | none (0.11) → none | 9 of 9 → 0 |
| Identity card, photograph in the portrait | `faces` 1, `personal_text` 6 | 0.94 → none | 9 of 9 → 0 |
| Two baseball players | `faces` 3 | 0.93, 0.57, 0.39 → none | — |

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
    let guardrail =
        Arc::new(ImagePrivacyGuardrail::for_model(gate, model.as_ref(), ["api.anthropic.com"]));
    LlmAgentBuilder::new("desk")
        .model(model)
        .before_model_callback(guardrail.before_model_callback())
        .build()
}
```

- **Locality.** `ImagePrivacyGuardrail::for_model` reads `Llm::data_locality()`. A model in
  this process or on loopback is not guarded, since nothing leaves.
- **Released parts.** A part the gate released carries a release annotation; it passes again
  only when its bytes hash to a digest that gate released, so a forged annotation is gated.
- **Rules.** Under the default `ask`, an image bound for a model's context is withheld with a
  placeholder; the vision tools can still read it on the device. An `allow` rule for the
  model's host and class `model_context` releases the redacted copy.
- **Boundary.** User code that calls an `Llm` directly, without the guardrail, is outside this
  guarantee.

## Invariants

| | Invariant | Enforced by |
|---|-----------|-------------|
| V-I1 | A remote backend receives only a `ReleasedImage`, and only the egress gate constructs one | The type system: remote backends implement their trait for `ReleasedImage` |
| V-I2 | Redaction precedes every release; no configuration releases before it | The gate's step order; `.adk/vision.toml` refuses unknown keys |
| V-I3 | Every release writes its record, with source and egress digests and the destination, before the request is sent | `EgressRecorder` and the run's audit trail (`vision.egress.released`) |
| V-I4 | With the vision pack, the guardrail is on for remote model calls, and no agent tool removes it | `adk-harness` feature `vision` |
| V-I5 | A held vision call carries its evidence, and a different image holds it again | The egress digest in the call's fingerprint |
| V-I6 | A priced call opens its hold before its request | The runtime's preflight, before any image is released |
| V-I15 | Records carry digests and artifact references, never pixels | `vision.*` records |
| V-I17 | A freeze refuses every vision call and every release | `VisionCx::check` at each step |

## Related

- [Governed Vision Tools](../vision/governed-tools.md)
- [Guardrails](guardrails.md)
- [Network Egress Policy](egress.md)
- [Tool Authorization](tool-authorization.md)
