# Harness Voice Surface

The voice surface runs the same `HarnessAgent` over a realtime session: voice in and out,
interruption, and spoken approvals, under the same policy (spec 3.1 §7.17, §5 H5). The harness
is the agent of an `IntegratedRealtimeRunner`: its tools are the runner's tools and
`HarnessAgent::run_config()` is the runner's `tool_governance`, so a spoken request runs under
the session's mode and the subject's standing rules, through the same governed path, action
ledger, spend ledger and audit trail as a typed one. A call that needs a decision is held,
surfaced as a realtime event, and answered by voice or in the approvals inbox. The pack is
wiring: `adk-realtime` carries the session, the hold, the metering and the guardrails.

> **Note:** the pack is gated on three realtime hardening rows, which ship with it in
> `adk-realtime`: realtime runners surface and resume a held call (a delegate's included),
> realtime usage is metered in the spend ledger, and realtime text passes the output
> guardrails.

## Quick start

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["voice"] }
adk-realtime = { version = "3.0.0", features = ["integration", "gemini"] }
adk-devtools = "3.0.0"
adk-session = "3.0.0"
adk-core = "3.0.0"
anyhow = "1"
```

```rust
use std::sync::Arc;

use adk_core::Llm;
use adk_harness::{HarnessAgent, Pack, VoiceConfig, VoiceEvent, VoiceSurface};
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_session::InMemorySessionService;

async fn talk(model: Arc<dyn Llm>, api_key: String) -> anyhow::Result<()> {
    let harness = Arc::new(
        HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
            .pack(Pack::Voice)
            .build()?,
    );
    let live = GeminiRealtimeModel::new(
        GeminiLiveBackend::studio(api_key),
        "models/gemini-3.1-flash-live-preview",
    );
    let surface = VoiceSurface::open(
        harness,
        Arc::new(live),
        Arc::new(InMemorySessionService::new()),
        "ana",
        "voice-1",
        VoiceConfig::new("adk-rust"),
    )?;
    surface.connect().await?;
    surface.say("what's failing in the build?").await?;
    while let Some(event) = surface.next_event().await {
        match event? {
            VoiceEvent::Held { request, .. } => {
                println!("{} needs your approval: say yes or no", request.tool_name);
            }
            VoiceEvent::Decided { decision, by, .. } => println!("{decision:?} by {by:?}"),
            VoiceEvent::Resumed(call) => println!("{} ran: {}", call.name, call.result),
            VoiceEvent::Provider(_) => {}
        }
    }
    Ok(())
}
```

The microphone and the speaker are the caller's: feed `VoiceSurface::send_audio` with PCM16 and
play the `ServerEvent::AudioDelta`s that `VoiceEvent::Provider` carries. `adk-rust --voice` does
both ([the CLI](#the-cli)).

## What the surface does

| Step | Mechanism |
|---|---|
| Builds the runner | `IntegratedRealtimeRunner::builder()` with every harness tool as an ADK tool, `harness.run_config()` as `tool_governance`, `agent_name` set to the harness's name, the session service for transcripts, and `concurrent_tool_dispatch(true)` so the event loop keeps carrying the user's speech while a command runs |
| Instructs the model | The harness prompt, the environment block, the workspace's instruction files as the harness renders them (inlined only once the workspace is trusted), the session's mode banner, and how to ask for a spoken decision |
| Governs each call | The runner's governed path, under the harness's name: `HarnessPolicy` classifies the call by the harness's tool classes and applies the session's mode and the subject's standing rules; the acting agent of each fingerprint and audit record is the harness; the harness `bash` sees the same run config and refuses without it (I17) |
| Holds a call | The run config carries no confirmation handler — a handler answers inside the call and would block the loop that carries the spoken answer — so a call needing a decision is held: answered to the model as `confirmation_pending`, recorded in the session and in the approval store, and reported as `VoiceEvent::Held` after `ServerEvent::ToolConfirmationRequested`. The model is instructed to say what the call would do and ask for yes or no |
| Decides by voice | A completed input transcript while a call is held is read by `parse_spoken_decision`: a plain approval or denial decides the oldest held call once; anything else leaves it held. The decision is recorded in the approval store under the request's fingerprint and the call resumes through `resume_held_calls()` under its original call ID |
| Decides in the inbox | The approval store is the one the approvals inbox writes; the surface polls it while a call is held, and `VoiceSurface::decide` is the same route in process |
| Audits | `harness.voice.held`, `harness.voice.decided` (decision, `by`, the transcript's digest) and `harness.voice.resumed` (`executed`) join the governed `ToolCall` record and `harness.bash.begin`/`end` in the run's audit trail; the transcript itself is a user event in the session |
| Meters | Each response's usage is reserved and committed in the harness spend ledger (`VoiceConfig::with_pricing` prices it; without pricing the run's `LlmSpendEstimate` is committed) |
| Memory | `store_to_memory` and memory injection are off: memory is written only through the memory tool under approval (§7.15, I33), and recalled context enters only through the harness instruction (I40). Transcripts persist as session events |

A spoken decision is allow-once or deny. A standing rule is created only at the console or in
the inbox (I2): a transcript is not the authenticated human action a rule requires.

## Parked kernel processes

With features `voice` and `kernel`, the surface also answers processes the
[agent kernel](../kernel/index.md) parked on a held call (spec 3.2 §7.11): a background goal's
write, a webhook-woken run's command. It decides them through the kernel's approval service,
whose run store wakes the parked run, and never through its own realtime session.

```rust
use std::sync::Arc;

use adk_core::{ApprovalService, RequestContext};
use adk_harness::{HarnessAgent, VoiceConfig, VoiceSurface};
use adk_kernel::Kernel;
use adk_realtime::model::BoxedModel;
use adk_session::SessionService;

async fn answer_parked(
    harness: Arc<HarnessAgent>,
    realtime: BoxedModel,
    sessions: Arc<dyn SessionService>,
    kernel: &Kernel,
    approvals: Arc<dyn ApprovalService>,
) -> adk_core::Result<()> {
    let context = RequestContext {
        user_id: "ana".into(),
        scopes: vec!["operator".into()],
        metadata: Default::default(),
    };
    let surface = VoiceSurface::open(
        harness,
        realtime,
        sessions,
        "ana",
        "voice-1",
        VoiceConfig::new("adk-rust").with_request_context(context),
    )?
    .with_parked_processes(kernel.processes().clone(), approvals);
    surface.connect().await?;
    if let Some(parked) = surface.parked_processes().await?.into_iter().find(|p| !p.frozen) {
        surface.offer_parked(&parked.pid).await?;
        surface
            .say(&format!("A background task wants to run {}. Ask me to approve it.", parked.request.tool_name))
            .await?;
    }
    Ok(())
}
```

| Method | Behaviour |
|---|---|
| `with_parked_processes(processes, approvals)` | Answers the kernel's processes through `approvals`, built with `StoreApprovalService::with_run_store` over the kernel's run store. The voice user decides as `SubjectRef::human(user_id)` in the request context's scopes, as the approvals REST maps a caller |
| `parked_processes()` | The processes waiting on a request in the voice user's inbox, oldest first; a freeze-parked one carries `frozen: true` |
| `offer_parked(pid)` | Puts the process up for the user's next plain yes or no while no call of the session is held; the decision is `VoiceEvent::Decided` with the pid as `call_id` and `by: voice` |
| `decide_process(pid, decision)` | The same decision from code |

The session's own held calls are answered first. A process a freeze parked is refused with
`harness.voice.frozen` and a `harness.voice.refused` record, from `offer_parked`,
`decide_process` and a spoken answer alike: only `unfreeze` wakes it (K-I21). A process not
parked on a request the voice user may decide is refused with `harness.voice.not_parked`.

## The CLI

```bash
adk-rust --voice "what's failing in the build?"       # microphone and speaker (feature voice-desktop)
adk-rust --voice --voice-input yes.wav "run the tests" # a recording answers the first held call
adk-rust --voice --voice-output reply.wav "status?"    # the model's audio to a WAV file
```

| Flag | Behaviour |
|---|---|
| `--voice [TEXT]` | The voice surface in the workspace, mode Ask by default; `TEXT` is the first turn |
| `--voice-input <WAV>` | Without a microphone: the mono PCM16 recording, at the provider's rate (16 kHz for Gemini Live, 24 kHz for OpenAI), is the user's answer to the first held call, sent once the model has asked about it (its first spoken response after the hold), at speaking pace and followed by 1.5 s of silence; the run ends after the decided call's response. Exit code 3 when a call is still held |
| `--voice-output <WAV>` | Appends the model's audio to the file instead of playing it |
| `--provider gemini\|openai` | The realtime provider: Gemini Live (`gemini-3.1-flash-live-preview`) or the OpenAI Realtime API |

The console prints `[you]` and `[harness]` transcripts, `approval needed:` for a held call with
the rendered command, and `decided (voice|inbox)` and `ran:` as the call resumes. Held calls
can also be answered from another terminal with `adk-rust approvals`, since the surface shares
the harness's approval store.

`voice` is an opt-in adk-cli feature; `voice-desktop` adds `adk-audio`'s cpal capture and
playback. With `--voice-input` and `--voice-output` a build without `voice-desktop` runs
end to end, which is what CI and the scripted tests use.

## Testing

| Where | Proves |
|---|---|
| `adk-harness/tests/voice_pack_tests.rs` | A scripted "yes" approves the held `bash` and it runs once, held as the harness's own call; "no" denies and nothing runs; an unclear answer leaves the call held; the inbox route resumes; harness tools keep their classes (`write_todos` runs in Ask without a prompt); ReadOnly denies without a hold; the memory policy, the instruction files and the spoken mode banner; an unmarked harness is refused |
| `adk-realtime/tests/realtime_hardening_tests.rs` | The hold and metering rows through APIs that exist on `main`, so each test fails there: a held call is answered as pending and recorded, a delegate's hold holds the agent-tool call, both runners commit realtime usage to the spend ledger |
| `adk-realtime/tests/integration_held_calls_tests.rs` | A held call is surfaced, recorded and resumed once; a delegate's hold holds the agent-tool call; concurrent dispatch keeps events flowing |
| `adk-realtime/tests/realtime_metering_tests.rs` | OpenAI and Gemini usage priced and committed; the estimate without pricing; a cap refuses the response |
| `adk-realtime/tests/integration_output_guardrails_tests.rs` | A transform rewrites the persisted transcript; a block interrupts and persists nothing |
| `examples/harness_agent` `voice_spoken_approval`, `live_voice_approval` | The H5 scenario scripted, and live on Gemini Live with a recorded answer |
| `adk-harness/tests/voice_kernel_tests.rs` | A scripted spoken yes wakes a kernel process parked on a held write, which runs once; a freeze-parked process is refused, stays parked with its request pending, and resumes only on unfreeze |
| `examples/harness_kernel` `v1_voice_answers_a_parked_process`, `live_voice_parked_process` | A parked process decided by voice, scripted and live on Gemini Live with a recorded answer |

The scripted provider is `adk_realtime::scripted::ScriptedRealtimeModel` (feature `scripted`).

## Related

- [Modes and approvals](modes-and-approvals.md) — the mode table, held calls and decisions
- [Serving the harness](serving.md) — the approvals inbox the surface shares
- [Realtime & Multimodal](../realtime/index.md) — the realtime stack, transports and providers
