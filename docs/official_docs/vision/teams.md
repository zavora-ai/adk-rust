# Perceiver, Reasoner, Verifier Teams

A multimodal team splits seeing from judging. A **perceiver** looks at the full image on the
device; a **reasoner**, usually a cloud model, judges from the regions it found and a redacted
crop; a **verifier**, by default on another provider, re-checks the reasoner's claim against
the pixels. When the reasoner and the verifier disagree, a person decides, with both opinions
and the image on one card. This page implements spec 3.7 §7.12 and the generic part of the
§7.14 verification loop ([spec](../../design/spec-3.7-vision.md)).

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["team"] }
```

> **Note:** the spec's open-question defaults are in force: images leave only after
> redaction, through the egress gate, and each remote host asks first unless a rule allows it
> (Q1); faces are filled by the YuNet face detector, and a person it finds no face for whole
> (Q3).

## The Three Roles

| Role | Model | Tools | Sees | Construction refuses |
|------|-------|-------|------|----------------------|
| `Perceiver` | On the device, or none | A `VisionToolset` whose runtime has only on-device backends, and tools that declare no network use | Full images on the device | A model whose `data_locality()` is `Remote`; a runtime with a remote backend; a tool that declares anything but `ToolEgress::None`; a vision tool given loose, whose runtime cannot be checked |
| `Reasoner` | Any; usually a cloud model | None | A `RegionSet`, measurements, and crops the egress gate released | A remote model without an egress gate |
| `Verifier` | Any; by default another provider than the reasoner's | Local perception: a runtime of on-device backends | The claim, its regions, the source image on the device | A runtime with a remote backend; a remote model without an egress gate |

What each role may hold is enforced where it is built, not documented:

- **The perceiver holds nothing that leaves the device.** `Perceiver::builder(..).build()`
  refuses every tool, model and backend in the table. Each tool it keeps is wrapped in
  `LocalOnlyTool`, which refuses a call whose declared egress is not `ToolEgress::None` before
  the tool runs, so a tool whose network use depends on its arguments cannot be steered off
  the device either.
- **The reasoner never receives an unredacted image.** Every crop goes through the egress
  gate: cropped to the region and a 16 px margin, redacted, decided under the egress rules and
  recorded as `vision.egress.released`. The request is then screened by an
  `ImagePrivacyGuardrail` on the same gate, so an image that did not pass it is released or
  replaced by a placeholder. The reasoner's agent form accepts only digests the gate released.
- **The verifier checks against the pixels.** Its on-device detector looks for the claimed
  class, and the best overlap with the claimed region confirms or rejects the claim. Without a
  model, that check decides; with one, the model sees the claim, the check and a released crop,
  and decides.

## A Team on One Image

`VisionTeam` runs the roles over one image:

| Perceiver score | What happens |
|-----------------|--------------|
| At or above the confident bound | Reported as the positive verdict (`FindingSource::Perceiver`) |
| In the ambiguous range, `[0.25, 0.75)` by default | A redacted crop goes to the reasoner; the verifier re-checks its claim |
| Below the range | Ignored |

When the reasoner and the verifier agree, the finding is theirs (`FindingSource::Agreed`).
When they disagree, the team holds one governed call, `vision_team_disagreement`, through
`adk_core::authorize_tool_call`:

| Part of the card | Content |
|------------------|---------|
| Arguments | The question, the frame's digest, the region, the reasoner's and the verifier's opinions (verdict, confidence, rationale, provider), the crop's egress digest, and what approving and denying mean |
| Evidence | The frame, stored as an artifact, with the claimed region and what the verifier found drawn over it; the crop the reasoner saw, after redaction |
| Approve | Accepts the reasoner's verdict |
| Deny | Keeps the verifier's verdict |

The run ends on that one card, and every surface draws it as an
[approve-with-image card](governed-tools.md). The opinions are kept in session state while the
card waits, so the resumed run asks the same call, the person's decision applies, and the
reasoner is not asked again (`FindingSource::Person`). A crop whose egress rule asks is held
first as `vision_team_release`, with the exact crop that would leave.

```rust,ignore
use std::sync::Arc;

use adk_vision::team::{Perceiver, Reasoner, Verifier, VisionTeam};

let perceiver = Perceiver::builder("perceiver").runtime(local_detector).build()?;
let reasoner = Reasoner::builder("reasoner")
    .model(claude)                       // claude-haiku-5-5
    .egress_gate(gate.clone())
    .hosts(["api.anthropic.com"])
    .build()?;
let verifier = Verifier::builder("verifier")
    .runtime(strict_local_detector)
    .model(nemotron)                     // nvidia/nemotron-3.5-super-vl-120b-a12b
    .provider("nvidia")
    .egress_gate(gate)
    .hosts(["integrate.api.nvidia.com"])
    .build()?;
let team = VisionTeam::builder("inspector")
    .perceiver(perceiver)
    .reasoner(reasoner)
    .verifier(verifier)
    .question("Is the mark a surface defect, or only a print artefact?")
    .verdicts("defect", "no_defect")
    .classes(["scratch"])
    .media_store(store)
    .build()?;
// Run it as any agent: the request names the image, by artifact name or digest.
// {"image": "part-0042.png"}
```

### The Provider Default

`VisionTeamBuilder::build` refuses a verifier on the reasoner's provider, so the check is
independent. The default is not a rule: `allow_same_provider()` lifts it. A provider is the
vendor a model id names (`claude-*` is `anthropic`), the label given with `provider(..)`, or,
for an id no vendor claims, the id itself; a verifier without a model is `local`.

## The Portable Team Template

`adk_agent::TeamArchitectureTemplate::PerceiveReasonVerify` lowers the same roles to a
`TeamSpec` for an LLM-directed coordinator. Each member carries its role's capability, and
each coordinator edge is isolated with no history and the role's input schema:

| Member | Capability | Edge input | Edge output |
|--------|------------|------------|-------------|
| Perceiver | `vision.perceive.local` | `perceiver_input_schema()`: an image and what to look for | Regions |
| Reasoner | `vision.reason.released` | `reasoner_input_schema()`: question, `RegionSet`, measurements, released crop digests — no field can carry pixels | `opinion_schema()` |
| Verifier | `vision.verify.pixels` | `verifier_input_schema()`: claim, image digest, regions | `opinion_schema()` |

`Perceiver`, `Reasoner` and `Verifier` are `Agent`s and bind to those members.

## In the Harness

With the vision pack, `adk-harness` has the three roles as member presets
([Harness Team](../harness/team.md)):

| Preset | Kind | Tools | Build refuses |
|--------|------|-------|---------------|
| `MemberSpec::perceiver(model)` | `MemberKind::Perceiver` | `MemberTools::LocalPerception`: the read tools and the pack's tools whose default backend is on the device | A remote model; `Full` or `Custom` tools; any extra tool that declares network use |
| `MemberSpec::reasoner(model)` | Local | None; answers an opinion | — |
| `MemberSpec::verifier(model)` | Local | `MemberTools::LocalPerception` | Any extra tool that declares network use |

Every local member whose model is remote gets the pack's privacy guardrail, so a reasoner on
Claude sees only images the gate released.

## The Verifier Loop

After an action or a claim, `VerifierLoop` compares a fresh observation with the one before
it, region by region:

| Step | What happens |
|------|--------------|
| Observe | The `Observer` takes the observation before the action |
| Act | The `VerifiedAction` acts and returns its `ExpectedChange`: regions that should appear or disappear |
| Check | `check_change` pairs regions of the same label by overlap; the check passes when every expected change happened and nothing else changed |
| Re-ground | A failed check goes back to the action once, with the report, to ground its target again and act again |
| Hold | A second failure returns `LoopOutcome::Held`; `VerificationHold::governed_call` is one `vision_verification_failed` call with both failed checks and the frames before and after as evidence |

```rust,ignore
use adk_vision::{LoopOutcome, VerifierLoop};

match VerifierLoop::new(screen_observer).run(&cx, &click_save).await? {
    LoopOutcome::Verified { attempts, .. } => tracing::info!(attempts, "verified"),
    LoopOutcome::Held(hold) => {
        adk_core::authorize_tool_call(&gate, hold.governed_call("verify-1")).await?;
    }
    _ => {}
}
```

## Verification

| Proof | Where |
|-------|-------|
| A perceiver given a remote-destination or undeclared tool, a remote model, a remote backend or a loose vision tool is refused at construction | `adk-vision/tests/team_tests.rs` |
| A perceiver call whose arguments would leave the device is refused when it runs | `team_tests.rs` |
| The bytes the recording reasoner received are the gate's released crop, 92×52, with the face filled, never the frame | `team_tests.rs` |
| A seeded disagreement raises exactly one card with both opinions, the frame's digest and the crop's; approval resolves it without a second card | `team_tests.rs` |
| Under an `ask` rule the crop is held first with the staged redacted image, and after approval the reasoner receives exactly that digest | `team_tests.rs` |
| A harness perceiver on a remote model or with a remote tool does not build | `adk-harness/tests/pack_vision_team_tests.rs` |
| Live: `claude-haiku-5-5` reasoner and NVIDIA NIM `nemotron-3.5-super-vl` verifier on a generated part with an ambiguous mark | `adk-vision/tests/team_live_tests.rs` (`#[ignore]`, needs `ANTHROPIC_API_KEY`, `NVIDIA_API_KEY`) |
| Scenario S15: the Inspector's detector as the perceiver on a conveyor frame, Claude and NIM through their real clients, YuNet screening each crop; one card, resolved on the resumed run, bound to the frame's and the crop's digests in the audit records | `examples/vision` (`s15_perceiver_reasoner_verifier`), run by the PR tier's `vision-scenarios` job; [the scenarios](scenarios.md) |
