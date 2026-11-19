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
the words it covers and their boxes. A class the rules require with no redactor installed
refuses every release, and a redactor refuses a remote backend.

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
