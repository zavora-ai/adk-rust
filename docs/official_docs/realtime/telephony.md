# Telephony

Phone calls for realtime voice agents: answer inbound calls, transfer and hang up, send DTMF,
and place outbound calls, with every action the model takes running through the governed tool
path.

| Provider | Feature | Inbound | Transfer | Hang up | DTMF | Outbound |
|----------|---------|---------|----------|---------|------|----------|
| OpenAI Realtime SIP | `openai-sip` | `realtime.call.incoming` webhook, accept / reject | SIP REFER | yes | not documented by OpenAI | not in this API |
| LiveKit SIP | `livekit-sip` | inbound trunk + dispatch rule; `participant_joined` webhook | `TransferSIPParticipant` | `RemoveParticipant` | while dialling (extension tones) | `CreateSIPParticipant` |

With LiveKit, the caller is a participant in a LiveKit room, and the LiveKit bridge (feature
`livekit`) joins the room with any realtime model — OpenAI Realtime, Gemini Live, Vertex Live.

## How a call flows

1. The phone network delivers the call to the provider over a SIP trunk.
2. The provider announces it; the application answers or declines it.
3. A `RealtimeAgent` runs on the call's realtime session. Its call-control tools act on that
   one call.
4. The call ends when the caller hangs up, the agent calls `end_call` or transfers the caller,
   the application hangs up, or a kill switch freezes.

## Call-control tools

The `telephony` feature (implied by each provider feature) provides the call controls as
`adk_core::Tool`s. Under a `RealtimeAgent`, or as an `adk_tool` of an
`IntegratedRealtimeRunner`, each call goes through `adk_core::authorize_tool_call` — the kill
switch, the `ToolPolicy`, confirmation — then the run's action ledger for the non-idempotent
ones, and the run's tool-call budget. A `RealtimeRunner` native handler gets the governed path
but neither the ledger nor the budget, so register the call tools as ADK tools.

| Tool | Name the model sees | Effect | What it does |
|------|---------------------|--------|--------------|
| `TransferCallTool` | `transfer_call` | `NonIdempotent` | Transfers the caller to a destination the application named; the model picks a name and never supplies a number |
| `EndCallTool` | `end_call` | `Idempotent` | Hangs up |
| `SendDtmfTool` | `send_dtmf` | `NonIdempotent` | Sends keypad tones, for example to choose an option in a phone menu |
| `PlaceCallTool` | `place_call` | `NonIdempotent` | Places a call to a contact the application named, at most `max_calls` times (one by default), attempts included |

Each tool declares its egress (the provider's API host) to the run's egress policy.

| Situation | What happens |
|-----------|--------------|
| The policy denies `transfer_call` | The model is answered `Tool 'transfer_call' denied by policy: <reason>`; no request reaches the provider |
| The policy requires approval and no decision exists | The call is held: `RealtimeAgent` emits the `ToolConfirmationRequest`; no request reaches the provider |
| A person approves | One REFER; the call is recorded in the action ledger, so the same call ID issued again is answered `already_succeeded` |
| The model names a destination that is not configured | `telephony.unknown_destination`, listing the configured names |
| `place_call` past `max_calls` | `telephony.call_limit` |
| The run's `RunBudget::max_tool_calls` is spent | The call is answered `not run` and the session ends with the `budget.*` error |
| A kill switch freezes | The agent's session ends with `governance.frozen`, and a `hang_up_on_freeze` guard hangs up the call |

### Requiring approval for transfers

```rust
use std::sync::Arc;

use adk_core::{DeclarativePolicy, RunConfig};

fn transfer_needs_a_person() -> RunConfig {
    let policy = DeclarativePolicy::builder()
        .allow("end_call")
        .require_approval("transfer_call", "a person approves transfers")
        .build();
    RunConfig::builder().tool_policy(Arc::new(policy)).build()
}
```

### Ending the call on a freeze

OpenAI carries the call's audio itself, so when the agent's session ends the model keeps
talking to the caller. `hang_up_on_freeze` hangs up the call as soon as any of the given kill
switches freezes, while its guard is held:

```rust
use std::sync::Arc;

use adk_realtime::telephony::{CallControl, CallFreezeGuard, hang_up_on_freeze};
use adk_runner::Runner;

fn guard_call(runner: &Runner, call: Arc<dyn CallControl>) -> CallFreezeGuard {
    hang_up_on_freeze(call, [runner.governance().clone()])
}
```

Hold the guard for as long as the call lives. `POST /api/admin/freeze` on `adk-server`, or
`runner.governance().freeze(reason)`, then ends every guarded call.

## OpenAI Realtime SIP

```toml
[dependencies]
adk-realtime = { version = "3.0.0", features = ["openai-sip"] }
adk-runner = "3.0.0"
adk-core = "3.0.0"
```

### Setup

1. **Find the project ID.** In the OpenAI dashboard, **Settings → Project → General** shows the
   project ID, which starts with `proj_`.
2. **Create a webhook endpoint.** Under **Settings → Project → Webhooks**, add an endpoint whose
   URL is the route your server exposes (for example
   `https://voice.example.com/openai/webhook`) and subscribe it to `realtime.call.incoming`.
   Copy the signing secret it shows once, and store it as `OPENAI_WEBHOOK_SECRET`.
3. **Point your SIP trunk at OpenAI.** In your SIP provider (Twilio, Telnyx, a PBX), route the
   phone number to the project's SIP URI:

   | Region | SIP URI |
   |--------|---------|
   | Global | `sip:<project id>@sip.api.openai.com;transport=tls` |
   | Europe (data residency) | `sip:<project id>@sip-eu.api.openai.com;transport=tls` |
   | India | `sip:<project id>@sip-in.api.openai.com;transport=tls` |

4. **Answer quickly.** The caller waits until your server accepts or rejects the call. Respond
   to the webhook with `200` at once and answer the call from a task: OpenAI expects a fast
   `2xx`, retries a delivery that fails for up to 72 hours with backoff, and treats a redirect
   as a failure.

### Answering a call

`OpenAIWebhookVerifier` checks the Standard Webhooks signature (`webhook-id`,
`webhook-timestamp`, `webhook-signature`; HMAC-SHA256 over `{id}.{timestamp}.{body}`), refuses
deliveries more than five minutes old or new, and, with a replay guard, a `webhook-id` it has
already accepted. A secret starting with `whsec_` is base64-decoded; any other secret is used as
its bytes, as OpenAI's SDKs do.

`OpenAISipClient` calls the documented endpoints:

| Method | Request |
|--------|---------|
| `accept(call_id, &RealtimeConfig)` | `POST /v1/realtime/calls/{call_id}/accept` with the session object `session.update` carries, plus `model`, and no audio format |
| `reject(call_id, Some(486))` | `POST /v1/realtime/calls/{call_id}/reject` with `{"status_code": 486}`; `None` lets OpenAI send `603 Decline` |
| `refer(call_id, &CallTarget)` | `POST /v1/realtime/calls/{call_id}/refer` with `{"target_uri": "tel:+14155550123"}` |
| `hang_up(call_id)` | `POST /v1/realtime/calls/{call_id}/hangup` |

`realtime_model(call_id)` returns an `OpenAIRealtimeModel` that attaches to the call's session
at `wss://api.openai.com/v1/realtime?call_id={call_id}` — no `model` parameter, and its
`session.update` leaves the audio format to the call. A `RealtimeAgent` on that model sends its
instructions and tools, receives the model's tool calls, and dispatches them through the
governed path.

```rust,no_run
use std::sync::Arc;

use adk_core::Content;
use adk_core::webhook::InMemoryWebhookReplayGuard;
use adk_realtime::openai::sip::{OpenAISipClient, OpenAIWebhookEvent, OpenAIWebhookVerifier};
use adk_realtime::telephony::{CallControl, CallTarget, EndCallTool, TransferCallTool, hang_up_on_freeze};
use adk_realtime::{RealtimeAgent, RealtimeConfig};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use futures::StreamExt;

const INSTRUCTION: &str = "You answer calls for Example Corp. Transfer billing questions.";

/// Handles one webhook delivery: `header` looks up a request header by its lowercase name.
async fn on_webhook<'a>(
    verifier: &OpenAIWebhookVerifier,
    header: impl Fn(&'static str) -> Option<&'a str>,
    body: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A failure here is answered with 400, and the body is not acted on.
    let OpenAIWebhookEvent::IncomingCall(call) = verifier.verify(header, body).await? else {
        return Ok(());
    };
    // Answer from a task so the webhook gets its 200 at once.
    tokio::spawn(async move {
        if let Err(error) = answer(call.call_id).await {
            tracing::error!(error = %error, "call failed");
        }
    });
    Ok(())
}

async fn answer(call_id: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = OpenAISipClient::new(std::env::var("OPENAI_API_KEY")?);
    let config = RealtimeConfig::default().with_instruction(INSTRUCTION).with_voice("marin");
    let call: Arc<dyn CallControl> = Arc::new(client.accept(&call_id, &config).await?);

    let agent = RealtimeAgent::builder("receptionist")
        .model(Arc::new(client.realtime_model(&call_id)))
        .instruction(INSTRUCTION)
        .voice("marin")
        .tool(Arc::new(
            TransferCallTool::new(call.clone())
                .destination("billing", CallTarget::phone("+14155550100")?),
        ))
        .tool(Arc::new(EndCallTool::new(call.clone())))
        .build()?;
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("front-desk")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()?;
    let _freeze = hang_up_on_freeze(call.clone(), [runner.governance().clone()]);

    let greeting = Content::new("user").with_text("Greet the caller and ask how you can help.");
    let mut events = runner.run_str("caller", &call_id, greeting).await?;
    while let Some(event) = events.next().await {
        if let Err(error) = event {
            // The model would keep talking without an agent: end the call.
            let _ = call.hang_up().await;
            return Err(error.into());
        }
    }
    Ok(())
}

fn verifier() -> Result<OpenAIWebhookVerifier, Box<dyn std::error::Error>> {
    Ok(OpenAIWebhookVerifier::new(&std::env::var("OPENAI_WEBHOOK_SECRET")?)?
        .with_replay_guard(Arc::new(InMemoryWebhookReplayGuard::new())))
}
```

With several replicas behind one webhook URL, use a shared `WebhookReplayGuard` so a
redelivery reaching another replica is refused too.

### What this API does not cover

OpenAI's Realtime SIP flow documents no outbound calling and no DTMF. Outbound SIP calls and
DTMF exist on OpenAI's GPT-Live API (`/v1/live/sessions`), whose session protocol
(`gpt-live-1`, delegation) `adk-realtime` does not implement.

## LiveKit SIP

```toml
[dependencies]
adk-realtime = { version = "3.0.0", features = ["livekit-sip", "integration"] }
adk-core = "3.0.0"
```

`livekit-sip` uses LiveKit's server API only (`livekit-api`), not the WebRTC stack; add
`livekit` for the bridge that carries the call's audio.

### Setup

1. **Route the number to LiveKit.** In your SIP provider, send inbound calls to your LiveKit
   SIP URI, `sip:<subdomain>.sip.livekit.cloud`. LiveKit Cloud shows it on the **Project
   settings** page; `lk project list --json` prints the `ProjectId` (`p_<subdomain>`).
2. **Create an inbound trunk** for the number, in E.164:
   `LiveKitSip::create_inbound_trunk("front desk", vec!["+15105550100".into()], options)`.
3. **Create a dispatch rule** that puts each caller in a new room:
   `LiveKitSip::create_dispatch_rule("call-", options)` with `options.trunk_ids` set to the
   trunk's ID (every inbound trunk when empty).
4. **Add a webhook.** LiveKit Cloud: **Settings → Webhooks → Create new webhook**, with your
   URL and a signing API key. Self-hosted: the `webhook` section of the server config, with
   `api_key` and `urls`. Deliveries are `application/webhook+json`, signed with a JWT in the
   `Authorization` header whose `sha256` claim is the body's hash.
5. **For outbound calls, create an outbound trunk** with your provider's trunk domain and
   numbers: `LiveKitSip::create_outbound_trunk("outbound", "<trunk domain>", numbers, options)`.

The rest of the SIP admin API (list, update, delete) is `LiveKitSip::sip()`, `livekit-api`'s
`SIPClient`.

### Taking a call

`LiveKitWebhookVerifier` verifies the token and the body hash, and returns
`LiveKitWebhookEvent::IncomingCall` for a `participant_joined` whose participant is an inbound
SIP participant (kind `SIP`, a `sip.ruleID`). The call carries the room, the participant
identity, and LiveKit's `sip.callID`, `sip.phoneNumber`, `sip.trunkPhoneNumber`, `sip.trunkID`
and `sip.ruleID` attributes. LiveKit has connected the caller by then, so `answer` returns the
caller's `LiveKitSipCall`; `decline` removes the caller, which hangs up.

```rust,no_run
use std::sync::Arc;

use adk_core::{DeclarativePolicy, GovernanceControl, RunConfig};
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::model::BoxedModel;
use adk_realtime::telephony::livekit::{LiveKitSip, LiveKitWebhookEvent, LiveKitWebhookVerifier};
use adk_realtime::telephony::{
    CallAnswerer, CallFreezeGuard, CallTarget, EndCallTool, TransferCallTool, hang_up_on_freeze,
};
use adk_realtime::RealtimeConfig;
use adk_session::SessionService;

/// Builds the runner for a call `body`/`authorization` announced, on `model` — any realtime
/// provider — with a kill switch that also hangs up the call.
async fn take_call(
    body: &str,
    authorization: &str,
    model: BoxedModel,
    sessions: Arc<dyn SessionService>,
    control: GovernanceControl,
) -> Result<Option<(IntegratedRealtimeRunner, CallFreezeGuard)>, Box<dyn std::error::Error>> {
    let key = std::env::var("LIVEKIT_API_KEY")?;
    let secret = std::env::var("LIVEKIT_API_SECRET")?;
    let LiveKitWebhookEvent::IncomingCall(call) =
        LiveKitWebhookVerifier::new(&key, &secret).verify(body, authorization)?
    else {
        return Ok(None);
    };
    let livekit = LiveKitSip::new(&std::env::var("LIVEKIT_URL")?, &key, &secret);
    let handle = livekit.answer(&call, &RealtimeConfig::default()).await?;

    let policy = DeclarativePolicy::builder()
        .allow("end_call")
        .require_approval("transfer_call", "a person approves transfers")
        .build();
    let runner = IntegratedRealtimeRunner::builder()
        .model(model)
        .identity("front-desk", "caller", &call.call_id)
        .session_service(sessions)
        .adk_tool(Arc::new(
            TransferCallTool::new(handle.clone())
                .destination("billing", CallTarget::phone("+14155550100")?),
        ))
        .adk_tool(Arc::new(EndCallTool::new(handle.clone())))
        .tool_governance(
            RunConfig::builder().tool_policy(Arc::new(policy)).governance(control.clone()).build(),
        )
        .build()?;
    Ok(Some((runner, hang_up_on_freeze(handle, [control]))))
}
```

Then join the call's room with the LiveKit bridge (feature `livekit`) and feed the caller's
audio to the runner, as the `livekit_gemini_bridge` example does with `bridge_gemini_input`.

### Placing a call

`LiveKitSip::dialer(trunk_id)` is an `OutboundDialer`: `CreateSIPParticipant` on the outbound
trunk, waiting until the callee answers, into the request's `room` attribute (or
`with_room`). A busy or unanswered callee is a `RealtimeError::ServerError` whose message
carries the SIP status. Tones for an extension go in the request's `dtmf` attribute (`0`–`9`,
`*`, `#`, and `w` for a half-second pause). Give a model the dialler only through
`PlaceCallTool`, with named contacts and a call limit:

```rust
use std::sync::Arc;

use adk_realtime::telephony::livekit::{LiveKitSip, ROOM_ATTRIBUTE};
use adk_realtime::telephony::{CallTarget, PlaceCallTool};

fn paging_tool(livekit: &LiveKitSip) -> adk_realtime::Result<PlaceCallTool> {
    Ok(PlaceCallTool::new(Arc::new(livekit.dialer("ST_outbound")))
        .contact("on-call engineer", CallTarget::phone("+14155550199")?)
        .attribute(ROOM_ATTRIBUTE, "incident-4101")
        .max_calls(1))
}
```

In-call DTMF needs a participant connected to the room (LiveKit's `publishDtmf`), which the
server API does not offer, so `LiveKitSipCall` reports DTMF unsupported.

## Verifying

The contract tests run against the documented payloads without an OpenAI account:

```bash
cargo nextest run -p adk-realtime --features openai-sip,scripted --test telephony_openai_sip_tests
```

| Test | Proves |
|------|--------|
| `the_documented_incoming_call_webhook_verifies` | The guide's `realtime.call.incoming` payload, signed, becomes an `IncomingCall` |
| `a_forged_or_tampered_delivery_is_rejected`, `a_stale_delivery_is_rejected`, `a_redelivered_webhook_is_refused_by_the_replay_guard` | Forged, altered, stale and repeated deliveries are refused |
| `accept_sends_the_session_with_the_model_and_no_audio_format`, `reject_refer_and_hangup_send_the_documented_bodies` | The call-control requests match the documented bodies |
| `the_session_attaches_to_the_call_without_a_model_or_audio_format` | The WebSocket joins `?call_id=` with no `model`, and its `session.update` has no audio format |
| `an_approved_transfer_refers_the_call_once_and_is_ledgered` | A simulated call: signed webhook, accept, held transfer, approval, one REFER, one ledger record |
| `a_freeze_hangs_up_the_call_and_refuses_the_transfer` | A freeze refuses the transfer and hangs up the call |

```bash
cargo nextest run -p adk-realtime --features livekit-sip,scripted --test telephony_livekit_sip_tests
```

| Test | Proves |
|------|--------|
| `dialling_creates_a_sip_participant_and_waits_for_the_answer` | `CreateSIPParticipant` carries the trunk, number, room, identity, DTMF and `wait_until_answered`, under a `sip.call` grant |
| `a_busy_callee_is_an_error_carrying_the_sip_status` | A Twirp error's `sip_status_code` reaches the caller |
| `transfer_refers_the_caller_and_hang_up_removes_them` | `TransferSIPParticipant` and `RemoveParticipant` name the room and the caller, under room-admin and `sip.call` grants |
| `trunks_and_dispatch_rules_are_provisioned` | The inbound trunk and an individual dispatch rule with a room prefix |
| `an_inbound_sip_participant_is_an_incoming_call`, `a_tampered_or_foreign_webhook_is_rejected` | Signed `participant_joined` webhooks, and refusals |
| `an_approved_transfer_refers_the_livekit_caller_once`, `a_freeze_removes_the_livekit_caller`, `place_call_dials_through_the_livekit_trunk` | The governed path on a LiveKit call |

## See also

- [Realtime Tools](tools.md#the-governed-path) — the governed path every realtime tool call takes
- [Tool Authorization](../security/tool-authorization.md) — policies, confirmations, the action ledger and the kill switch
