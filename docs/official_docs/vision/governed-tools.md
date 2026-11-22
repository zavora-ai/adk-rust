# Governed Vision Tools

`VisionToolset` gives an agent nine vision tools that run on the governed tool path. A
call that stays on the device runs like any read. A call that would send an image off the
machine is checked against the egress rules, held for a person with the exact image it
would send when the rules ask, and released only as that redacted copy. Every backend
call opens its budget hold before any image leaves, and every call, release and edit
writes a signed `vision.*` audit record. This page implements spec 3.7 §7.8, §7.9 and
§9.3 ([spec](../../design/spec-3.7-vision.md)).

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["guardrail"] }
```

> **Note:** the spec's open-question defaults are in force: images leave only after
> redaction and each remote host asks first (Q1). Q3 is settled: faces are redacted by the
> YuNet face detector, and a person it finds no face for as the whole person.

## The Tools

| Tool | Does | Effect | Arguments |
|------|------|--------|-----------|
| `vision_detect` | Finds objects by class or concept | `ReadOnly` | `image`, `classes` or `concepts`, `region`, `backend` |
| `vision_segment` | Segments from concepts or box prompts | `ReadOnly` | `image`, `concepts`, `boxes`, `region`, `backend` |
| `vision_read` | Reads a document: blocks, words, tables | `ReadOnly` | `image`, `markdown`, `region`, `backend` |
| `vision_describe` | Answers a question, as text or a value matching a schema | `ReadOnly` | `image`, `question`, `schema`, `region`, `backend` |
| `vision_crop` | A new image of one region | `Idempotent` | `image`, `region`, `margin` |
| `vision_redact` | A new image with regions filled solid black | `Idempotent` | `image`, `regions` |
| `vision_annotate` | A new image with numbered boxes drawn | `Idempotent` | `image`, `boxes` |
| `vision_measure` | Maximum length and area in millimetres from masks, under a fiducial's calibration ([measurement](measure.md)) | `ReadOnly` | `image`, `boxes` or `concepts`, `fiducial` or `calibration`, `region`, `backend` |
| `vision_watch` | Lists the registered frame sources; starts or stops a watch condition that wakes a sleeping agent ([streams](streams.md#frame-sources-and-vision_watch), feature `stream`, with `VisionToolsetBuilder::frame_sources`) | `Idempotent` | `action`, `source`, `name`, `labels`, `minScore`, `minFrames`, `zone`, `zoneAnchor`, `cooldownSeconds` |

- **Images.** An `image` argument is an artifact name in the session or a `sha256:`
  digest an earlier tool returned. The toolset keeps each image it reads in the runtime's
  `MediaStore`, named by digest, so the digest in a call's arguments names one exact image.
- **Regions.** `region` is `[x0, y0, x1, y1]` in the image's pixels. With a region, the
  backend is sent that crop only, and every answer comes back in the image's pixels.
- **Tools offered.** Only the tasks the runtime has a backend for become tools.
- **Data classes.** The operator sets the class an image is released as —
  `VisionToolsetBuilder::data_classes("document", "document_crop")` for whole images and
  crops — never the model, because the egress rules are written per class.

## Wiring

Three pieces go on the agent and the run:

| Piece | Where | Why |
|-------|-------|-----|
| `toolset` | `LlmAgentBuilder::toolset` | The tools |
| `toolset.evidence_callback()` | `LlmAgentBuilder::before_tool_callback` | A held remote call carries the image it would send |
| `toolset.egress_policy()` | `Runner::builder().tool_policy` or `RunConfig::tool_policy` | The egress rules decide each remote call: `deny`, `ask` or `allow` |

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_core::egress::EgressPolicy;
use adk_runner::Runner;
use adk_vision::{
    Describer, Detector, DocumentReader, EgressGate, InMemoryEgressRecorder, MediaStore,
    PersonRedactor, PersonalTextRedactor, ReleasedImage, VisionConfig, VisionRuntime,
    VisionToolset,
};

/// `rules` is `.adk/vision.toml`, read with `VisionConfig::from_toml_str`.
fn desk(
    rules: VisionConfig,
    model: Arc<dyn adk_core::Llm>,
    people: Arc<dyn Detector>,
    ocr: Arc<dyn DocumentReader>,
    claude: Arc<dyn Describer<ReleasedImage>>,
    sessions: Arc<dyn adk_session::SessionService>,
) -> adk_core::Result<Runner> {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone());
    let gate = EgressGate::new(
        EgressPolicy::public_only(),
        rules.egress,
        Arc::new(InMemoryEgressRecorder::new()),
    )
    .with_redactor(Arc::new(PersonRedactor::new(people)))
    .with_redactor(Arc::new(PersonalTextRedactor::new(ocr.clone()).with_labelled_fields(["Name"])))
    .with_media_store(store.clone());
    let runtime = VisionRuntime::builder()
        .reader(ocr)
        .remote_describer(claude)
        .egress_gate(gate)
        .media_store(store)
        .build()?;
    let toolset = VisionToolset::builder(Arc::new(runtime))
        .data_classes("document", "document_crop")
        .build()?;
    let agent = LlmAgentBuilder::new("desk")
        .model(model)
        .toolset(Arc::new(toolset.clone()))
        .before_tool_callback(toolset.evidence_callback())
        .build()?;
    Runner::builder()
        .app_name("desk")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .artifact_service(artifacts)
        .tool_policy(toolset.egress_policy())
        .build()
}
```

A run that holds remote calls without the egress policy, or without the evidence callback,
fails closed: the call is refused with the missing piece named, and nothing leaves.
`EgressApproval::Explicit` is the alternative for an application that shows the image and
records the decision itself, through `VisionToolset::approve_egress`.

## Approve with the Image

For a call routed to a remote backend, the evidence callback runs the egress gate's first
three steps before the call is authorized — destination, minimise, redact — and stores the
redacted copy as an artifact (`EgressGate::stage`). The tool's
`approval_evidence` hands that copy to the governed path as `ImageEvidence`:

| Field | Value |
|-------|-------|
| `digest` | `sha256:` of the exact bytes that would leave |
| `artifact` | The stored egress copy, loaded by a surface with the viewer's own authorisation |
| `regions` | Each filled box, as a `RegionSet` labelled `faces`, `personal_text` or `marked` |
| `caption` | `to api.anthropic.com as document_crop · 1 face · the whole person is covered, 6 personal details covered` |

The digest joins the call's fingerprint, so a decision binds to that image:

1. Under an `ask` rule the egress policy returns `RequireApproval`, and the call is held
   with the evidence.
2. A person approves; the call runs and releases exactly the approved digest — the gate
   re-runs the steps and releases only when the new copy has that digest.
3. If the image changed before the call ran — a new version of the artifact — the
   staged copy has a new digest, the fingerprint differs, the call is held again, and the
   earlier decision stays unspent.

The signed audit trail links the decision and the image: the call's `tool_call` and
`approval` records carry `evidenceDigests`, and the `vision.egress.released` record names
the source and egress digests.

Every surface draws the held call with that image — the web home and the phone show it with
its regions, the terminal as text with a link, chat attaches it or links to it — and an answer
that lets the call run names the digest it saw: see
[Approve with the Image](../surfaces/approve-with-image.md).

## Egress Rules

The rules come from `.adk/vision.toml`; see [The Egress Gate](index.md#the-egress-gate).
The egress policy reads them per call:

| Rule for the host and data class | Policy decision | Gate decision |
|----------------------------------|-----------------|---------------|
| `deny` | `Deny`; the evidence callback refuses before anyone is asked | `vision.egress.refused` at `decide` |
| `ask` | `RequireApproval`, with the egress image as evidence | Releases the approved digest only |
| `allow` | `Allow` | Releases the redacted copy |
| — (on-device backend) | `Allow` | Nothing leaves |

Redaction precedes every decision: an `allow` rule releases the redacted copy, never the
original. An escalation from an on-device backend to a remote one is decided by the same
rules when the runtime escalates; under an `ask` rule its crops are not pre-approved and
the call fails with the egress digest, so give the escalation's host and class an `allow`
rule to escalate unattended.

## Budgets

Every backend call opens its hold before its image is prepared or released, so a call the
budget cannot cover is refused before any request and leaves no egress record (V-I6):

| Backend | Hold | Committed |
|---------|------|-----------|
| On-device | Images (and pages, for reading) at zero cost | The same, at zero cost (V-I7) |
| Per unit (`PriceRef::PerUnit`), such as `sam-3.1` | The unit price, with any per-request minimum | The unit price |
| Per token (`PriceRef::Tokens`) | Its `TokenEstimate`: image tokens from the sent size, prompt tokens, and the output cap | Zero; the model call commits its own tokens |

`ImageTokens::Claude` counts `⌈w/28⌉ × ⌈h/28⌉` tokens, `ImageTokens::GeminiTiles` 258 for
an image whose sides are at most 384 px, otherwise 258 per 768×768 tile. A run capped with
`RunBudget::max_units(UsageUnit::Images, 20)` makes 20 calls; the 21st is refused before
its request, naming the unit and the cap.

## Audit Records

Each record has kind `vision`, the acting agent as actor, and content by digest only:

| Action | Written | Fields |
|--------|---------|--------|
| `vision.call` | After each backend call | `tool`, `task`, `backend`, `locality`, `imageDigests`, `egressDigests`, `units`, `costMicroUsd`, `regions`, `durationMs`, `escalation` |
| `vision.egress.released` | Before the request is sent (V-I3) | `sourceDigest`, `egressDigest`, `chain`, `redactions`, `destination`, `dataClass`, `rule`, `decisionKind`, `estimatedCostMicroUsd`, `artifact` |
| `vision.egress.refused` | At the refusing step | `sourceDigest`, `egressDigest`, `step`, `reason`, `destination`, `dataClass` |
| `vision.edit` | After an edit | `inputDigest`, `operationDigest`, `outputDigest` |

They go to the run's audit trail (`RunConfig::audit`, signed when its recorder signs)
through `VisionCx::with_audit`; the gate also writes each release and refusal to its
`EgressRecorder`.

## The Harness Vision Pack

`adk-harness` feature `vision` installs the toolset, the rules and the privacy guardrail:

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["vision"] }
```

```rust
use std::sync::Arc;

use adk_harness::HarnessAgent;
use adk_harness::packs::vision::VisionPack;
use adk_vision::{
    Describer, Detector, DocumentReader, MediaStore, PersonRedactor, PersonalTextRedactor,
    ReleasedImage, VisionRuntime,
};

fn harness(
    model: Arc<dyn adk_core::Llm>,
    workspace: adk_devtools::Workspace,
    store: MediaStore,
    people: Arc<dyn Detector>,
    ocr: Arc<dyn DocumentReader>,
    claude: Arc<dyn Describer<ReleasedImage>>,
) -> adk_core::Result<HarnessAgent> {
    let reader = ocr.clone();
    let pack = VisionPack::new(store, move |gate, store| {
        VisionRuntime::builder()
            .reader(reader)
            .remote_describer(claude)
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    })
    .redactor(Arc::new(PersonRedactor::new(people)))
    .redactor(Arc::new(PersonalTextRedactor::new(ocr).with_labelled_fields(["Name"])))
    .model_hosts(["api.anthropic.com"]);
    HarnessAgent::builder("desk").model(model).workspace(workspace).pack_vision(pack).build()
}
```

| Piece | The pack |
|-------|----------|
| Classes | On the device `Read`; off it `External`, trusted read-only under an `allow` rule — so an `ask` call is held in Ask and Auto and denied in ReadOnly and Plan |
| Rules | `.adk/vision.toml`, or `VisionPack::config`; faces and personal text are always redacted, and a workspace file's `allow` rules ask until the operator calls `trust_workspace_rules` |
| Redactors | The operator's `VisionPack::redactor`s; with feature `vision-local`, a default for each required class none covers (below) |
| Approvals | The egress policy joins the operator policy; the harness prompter or inbox sees the egress image on the held call |
| Privacy guardrail | On every request to a remote harness model; no agent tool removes it ([Image Privacy](../security/image-privacy.md)) |
| Text | `vision_read` and `vision_describe` deliver recognised text inside an `<untrusted-content source="vision">` block, screened as fetched text is |

### Default Redactors

With `adk-harness` feature `vision-local`, the pack installs a default redactor for each class
the rules require that no `VisionPack::redactor` covers, so a pack needs no redactor wiring:

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["vision-local"] }
```

| Class | `vision-local` | Without it |
|-------|----------------|------------|
| `faces` | `FaceRedactor::on_device` — YuNet faces, RF-DETR Nano for people with no face found | No redactor: every remote release is refused |
| `personal_text` | `PersonalTextRedactor::pp_ocr` — PP-OCRv6 small, with `PERSONAL_FIELD_LABELS` | No redactor: every remote release is refused |

The weights load on the first release through `VisionPack::model_fetcher`, or a fetcher built
from `HF_HUB_CACHE`, `HF_HUB_OFFLINE` and `HF_TOKEN`, each file checked against the built-in
manifest. When they cannot load, that release is refused and recorded at the redact step;
when only YuNet cannot load, each detected person is covered whole. With `on_device = true`
in `[egress]` the default fetcher works offline and every release is refused at the
destination step ([The On-Device Profile](../security/image-privacy.md#the-on-device-profile)).
Calls that stay on the device never load them. `vision-local` builds ONNX Runtime, downloaded
at build time as `adk-vision`'s `onnx` feature does.

## Related

- [The Annotated Viewer](../surfaces/annotated-viewer.md) — each call's answer record, drawn over its image on the web home
- [Vision overview](index.md)
- [Image Privacy](../security/image-privacy.md)
- [Tool Authorization](../security/tool-authorization.md) - "Approving with Evidence"
- [Audit Log](../security/audit-log.md)
- [Spend Ledger](../security/spend-ledger.md)
