# The website runtime

Feature `website` of `adk-harness` is the runtime half of an AWP 2.0 website (spec 3.5 §7.1):
the preset, the tools and policies a site's team of agents works with, and the onboarding
machinery a product builds its plan from. It holds no product: the team, the site kinds, the
onboarding questions and the themes belong to a product such as `adk-website`.

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["website"] }
```

| Part | Types | Spec |
|---|---|---|
| Preset | `Preset::Website`, `WEBSITE_SYSTEM_PROMPT` | §7.1, §10 X10 |
| Tool catalogue | `CATALOG`, `ToolSpec`, `WebsiteClasses` | §7.10, §7.11, §7.13 |
| Site runtime | `SiteRuntime`, `SiteApprovals`, `FactClaimCheck` | §7.8, §7.8.2, A-I18, A-I23 |
| Back office | `back_office_tools`, `WebsitePolicy` | §7.8.2, §7.10, A-I13, A-I21 |
| Public partition | `SearchSiteTool`, `EmitSignalTool`, `PublicPolicy`, `screen_bounded` | §7.10, A-I2, A-I3 |
| Partitions | `SitePartitions`, `PublicKit`, `ServedPublicTools`, `BackOfficeKit`, `website_public`, `website_public_served`, `website_back_office` | §7.9, §7.10, §7.19, A-I22 |
| Autonomy ladder | `LadderCompiler`, `JobBinding`, `level_decision`, `promote_job`, `apply_demotion` | §7.11, A-I11, A-I13, A-I15 |
| Authoring | `AuthoringRouter`, `inbound_signal` | §7.8.2, A-I24 |
| Outbound signing | `SiteSigning`, `WebBotAuthSigner`, `HarnessAgentBuilder::web_signer` | §7.3 |
| Verification | `SiteVerifier`, `ReleaseLogVerifier`, `adk_awp::verify::Verifier` | §7.7, A-I9 |
| Onboarding | `OnboardingStage`, `onboarding_plan`, `InterviewForm`, `CONNECTION_NOTICE` | §7.18, A-I18, A-I19 |

## The preset

`Preset::Website` sets the website prompt, plan mode — every job proposes
until the charter's authority compiles to more — no shell and no default members, and classes
the website tools by their catalogue rather than by their own hints. `website_back_office` and
`website_public` apply it and attach the toolsets, which need the site's runtime.

A website agent's mode is its jobs' autonomy, which only the owner raises (A-I13), so the
preset offers no `exit_plan_mode`, and its mode banner says what the level allows: at Propose,
"draft every change with the drafting tools and raise decision cards; drafts and cards run in
this mode". A Propose agent drafts without asking to leave plan mode.

## Two partitions

| | Public | Back office |
|---|---|---|
| Partition | `{site}.public`, read-only mode | `{site}.back-office`, plan mode until the charter compiles to more |
| Principals | `urn:adk:agent:{site}/{agent}`, tenant `{site}` | `urn:adk:agent:{site}/{agent}`, tenant `{site}` |
| Memory | `{site}/public`: no owner facts | `{site}/back-office`: the owner's facts |
| Tools | Declared capabilities, `search_site`, `reply_in_thread` (`adk-drivers`), `emit_signal` | The back-office toolset |
| Policy | `PublicPolicy`: the manifest's fixed list, no standing rules | The partition policy, `WebsitePolicy`, the ladder with job pauses, `adk-drivers`' `DriverCapPolicy` |

A public agent cannot publish, deploy, change DNS, spend, send in bulk, fetch the web or read
owner memory, whatever a model asks: `PublicPolicy` denies every tool outside its list, even
one attached by mistake (A-I2). The harness offers a public agent none of its own tools — no
`read_file`, `write_file`, `edit_file`, memory, plan or web tool — only the partition's.

When another runtime serves the public tools, as the AWP edge serves its `PublicTools`,
`website_public_served` builds the agent over those instead of the kit's own `search_site`
and `emit_signal`: `ServedPublicTools` carries them, each is classed by its own origin so the
manifest policy decides it, and a declared `search_site` capability replaces the served one. `emit_signal` files a typed signal in the back office's agenda
through the kernel's `SignalSink`; every text field is screened by the injection screen,
redacted of secrets and personal data, cut to 280 characters and marked untrusted, and a
field that matches an injection pattern keeps only the screening label (A-I3). A back-office
request for the raw text behind a signal, `signal_source_text`, is a crossing into the public
partition, which the public partition refuses.

## The back-office toolset

| Tool | Effect class | Draft | Reversible | Held in every mode |
|---|---|---|---|---|
| `content_draft`, `content_edit`, `translation_draft`, `media_intake`, `charter_draft`, `release_preview`, `site_discover`, `interview_record` | `write_local` | yes | yes | — |
| `content_schedule`, `content_correct`, `content_withdraw`, `content_revert` | `publish_public` | — | — | — |
| `site_publish` | `publish_public` | — | — | the launch: the site's first production release (A-I21) |
| `site_rollback` | `publish_public` | — | yes (A-I11) | — |
| `edge_read_only` (the product's edge) | `publish_public` | — | yes (A-I11) | — |
| `experiment_start` (the product's experiment runner) | `publish_public` | — | — | — |
| `lead_handoff` (the product's alarm router) | `write_local` | — | — | — |
| `host_fetch`, `launch_gate_run`, `awp_verify`, `experiment_report` | `read` | — | — | — |
| `charter_commit_request`, `job_promotion_request` | `write_local` | — | — | always: each is a decision card |
| `wallet_pay` (`adk-payments`) | `financial` | — | — | — the wallet holds a payment over a cap |

Every tool calls `adk-site`'s publishing pipeline, launch gate, host and media pipeline. A
product implements `edge_read_only`, `experiment_start`, `experiment_report` and
`lead_handoff` under `BACK_OFFICE_ORIGIN`, so the catalogue classes them and a standing rule
pins them. `content_draft`, `content_edit` and `translation_draft` take an optional `draft`:
the item joins that draft, so one release carries several items, such as the pages of
release 1; `media_intake` takes the `item` an upload belongs to. A
claim cites the facts it rests on; `FactClaimCheck` lets it publish only when each is `Known`
in the site's scope (A-I18). A release under a charter approval chain publishes only when
distinct people hold every role and none proposed it (A-I23); `SiteApprovals::record_approval`
records their approvals and refuses an agent's vote or one in a role its voter does not hold.
A vote that cannot count — recorded with `approve`, or left over from a charter change — is
left out when the release is decided, so it never holds up the people who can approve. `charter_commit_request` commits nothing: `SiteRuntime::sign_charter` commits
the draft once every required signer has signed. Every DNS change is held by `adk-drivers`'
`DriverCapPolicy`.

## The autonomy ladder

| Level | Mode | Website tools of the job |
|---|---|---|
| Observe | `read_only` | read-only tools only |
| Propose | `plan` | read-only tools, drafts and decision cards |
| Act with approval | `ask` | every other call held for the owner |
| Act, reversible only | `auto` | drafts, a rollback and the edge's read-only switch run; every other call held |
| Act within budget | `auto` | drafts and reversible acts run; every other call runs within the job's `DeclarativePolicy` rules and run budget, and is held beyond them |

The ladder policy reads each job's level from the kernel's `AutonomyStore` at every call, so a
demotion applies to the next call and its reason appears in the refusal. A level above Propose
runs a call only through a standing rule: `promote_job` raises a job by an owner's decision —
the kernel refuses an agent, a service or a person without the owner role — and grants the
rules the new level holds, created by that owner, revoking the old level's rules the new one
does not hold (A-I13):

| Level | Standing rules |
|---|---|
| Observe, Propose | none: drafts and decision cards need no rule |
| Act with approval | the job's tools that are not read-only, drafts or decision cards; the ladder still holds each call, so the rules record the owner's grant |
| Act, reversible only | the rollback and the edge's read-only switch (A-I11) |
| Act within budget | the job's tools that are not read-only, drafts or decision cards |

A tool outside the catalogue, such as a driver's, is pinned to the origin the agent is given:
`LadderCompiler::with_tool_origins` records it. `apply_demotion` revokes the rules a
lower level no longer warrants. A charter's `spendCap` bounds the `amount` of a job's
financial calls, and its `actionsPerDay` caps the tool calls of a run.

```rust,ignore
use std::sync::Arc;

use adk_core::{Llm, Result};
use adk_devtools::Workspace;
use adk_harness::HarnessAgent;
use adk_harness::website::{
    BackOfficeKit, JobBinding, LadderCompiler, SitePartitions, SiteRuntime, job_levels,
};
use adk_kernel::Partitions;
use adk_kernel::charter::AutonomyStore;
use awp_types::Charter;

async fn steward(
    partitions: SitePartitions,
    table: Partitions,
    runtime: Arc<SiteRuntime>,
    charter: &Charter,
    autonomy: Arc<dyn AutonomyStore>,
    model: Arc<dyn Llm>,
    workspace: Workspace,
) -> Result<HarnessAgent> {
    let site = runtime.site().clone();
    let ladder = LadderCompiler::new(site.clone()).with_charter(
        charter,
        [JobBinding::new("launch", "steward", ["content_*", "release_preview", "site_publish"])],
    );
    let levels = job_levels(&ladder, autonomy.as_ref(), site.id).await?;
    let mode = ladder.mode_for_agent("steward", &levels);
    let kit = BackOfficeKit::new(partitions, table, runtime, ladder, autonomy);
    HarnessAgent::builder("steward")
        .model(model)
        .workspace(workspace)
        .website_back_office(&kit)
        .mode(mode)
        .build()
}
```

## Who authors content

`AuthoringRouter` routes an inbound message by the kernel's channel pairings: an account
paired to a person reaches the Steward in the back office, and its drafts record the paired
device as their entry point; any other account reaches the public partition, which can answer
in the thread and send an `inbound_message` signal but never author content (A-I24).

## Outbound Web Bot Auth

`SiteSigning` holds the site's Ed25519 key and the origin that publishes its key directory.
`site_discover` crawls and `awp_verify` requests are signed whenever the runtime has the
signer; general `web_fetch` calls are signed only when the operator opts in with
`HarnessAgentBuilder::web_signer`. The governed fetch signs every redirect hop again, since the
authority and path change.

## Onboarding

`onboarding_plan` turns `OnboardingStage`s into plan-graph steps: discover, interview, charter
draft, charter sign, build, gate, connect and deploy. Signing, connecting and launching are
`Approve` gates. Discovery records what it finds as `Assumed` facts citing their source;
`interview_record` records an answer as `Known` only when its quote is the owner's own words
in the session (A-I18). The connect step tells the owner, before the first connection, that
the site never asks for a password and connects only through OAuth, the credential broker or
pairing (A-I19).
