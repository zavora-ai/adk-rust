# Tool Effects and the Action Ledger

Every tool declares what calling it does to the outside world. The runtime uses that
declaration to decide whether a failed call may be retried, whether the call is recorded
in an action ledger, and what the model is told when a call ends without a result.

## Tool effects

`Tool::effect()` returns a `ToolEffect`:

| Effect | Meaning | Retried after a retryable error | Recorded in the action ledger |
|--------|---------|---------------------------------|-------------------------------|
| `ReadOnly` | Reads without changing anything | Yes | No |
| `Idempotent` | Changes state; repeating a call leaves the same state as making it once | Yes | No |
| `NonIdempotent` | Repeating a call repeats the change — a payment, an email, an order | No | Yes, when a ledger is configured |

The default derives the effect from `is_read_only()`: a read-only tool is `ReadOnly`, and
every other tool is `NonIdempotent`. A tool that declares nothing is therefore never
retried.

Declare the effect with the `#[tool]` macro, the `FunctionTool` builder, or a `Tool`
implementation:

```rust
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use adk_tool::{AdkError, FunctionTool, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
struct StatusArgs {
    ticket: String,
    status: String,
}

/// Sets a ticket's status. Setting the same status twice changes nothing.
#[tool(idempotent)]
async fn set_ticket_status(args: StatusArgs) -> std::result::Result<Value, AdkError> {
    Ok(json!({ "ticket": args.ticket, "status": args.status }))
}

let archive = FunctionTool::new("archive_ticket", "Archives a ticket", |_ctx, args| async move {
    Ok(args)
})
.with_effect(ToolEffect::Idempotent);

struct ChargeCard;

#[async_trait::async_trait]
impl Tool for ChargeCard {
    fn name(&self) -> &str {
        "charge_card"
    }
    fn description(&self) -> &str {
        "Charges the customer's card"
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        // Downstream payment APIs deduplicate on this key.
        let key = ctx.idempotency_key();
        Ok(json!({ "charged": args["amount"], "idempotency_key": key }))
    }
}
```

The payment tools in `adk-payments` declare `payments_checkout_create` and
`payments_checkout_complete` as `NonIdempotent` and pass the call's idempotency key to
the commerce backend in the `idempotency_key` extension field.

## Retries

`LlmAgent` and `CodeActAgent` retry a failed call under a `RetryBudget` only when both
hold:

1. The tool's effect is `ReadOnly` or `Idempotent`.
2. The error is retryable — `AdkError::is_retryable()` is true (rate limited,
   unavailable, timed out), or the call hit the agent's tool timeout.

The delay before retry `n` is `delay * 2^(n - 1)`, capped at `max_delay`, and drawn from
the upper half of that range so callers that failed together do not retry in lockstep.
Each attempt runs with a fresh tool context, so the state delta and escalation of an
attempt that is retried are never committed.

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::RetryBudget;
use std::{sync::Arc, time::Duration};

let agent = LlmAgentBuilder::new("support")
    .model(model)
    .tool(Arc::new(SetTicketStatus))
    .default_retry_budget(
        RetryBudget::new(3, Duration::from_millis(200)).with_max_delay(Duration::from_secs(5)),
    )
    .build()?;
```

## Idempotency keys

`ToolContext::idempotency_key()` identifies one tool call across every attempt and
replay. The default is `"{app}/{user}/{session}/{invocation}/{function_call_id}"`. When a
provider assigns no call ID, `LlmAgent` assigns one unique to the model turn, so two calls
in one invocation never share a key.

## The action ledger

An `ActionLedger` records every `NonIdempotent` call before it executes and its outcome
after. Set one on `RunConfig`:

```rust
use adk_core::{InMemoryActionLedger, RunConfig};
use std::sync::Arc;

let config = RunConfig::builder()
    .action_ledger(Arc::new(InMemoryActionLedger::new()))
    .build();
```

`LlmAgent` consults the ledger under the call's idempotency key before executing:

| Ledger state | Behaviour | Function response |
|--------------|-----------|-------------------|
| No record | `begin`, execute, `complete` | The tool's result |
| Completed with a result | Not executed | `{"status": "already_succeeded", "result_digest": "fnv1a128:…"}` |
| Completed with an error | Not executed | `{"status": "already_failed", "error": "…"}` |
| Begun, never completed | Not executed | `{"status": "outcome_unknown", "detail": "…"}` |
| Read or `begin` fails | Not executed | `{"error": "tool '…' was not executed: …"}` |

A call that times out or panics keeps its begun record, so a replay of it after a crash,
a timeout, or a cancelled run is answered as `outcome_unknown` rather than executed a
second time. The ledger stores `json_digest` digests of the arguments and result — a
stable identifier, not a cryptographic hash — never the payloads themselves.

| Implementation | Crate | Survives a restart |
|----------------|-------|--------------------|
| `InMemoryActionLedger` | `adk-core` | No |
| `SqliteActionLedger` | `adk-session` (`sqlite` feature) | Yes |

```toml
[dependencies]
adk-session = { version = "3.0.0", features = ["sqlite"] }
```

```rust
use adk_core::RunConfig;
use adk_session::SqliteActionLedger;
use std::sync::Arc;

let ledger = SqliteActionLedger::new("sqlite:actions.db?mode=rwc").await?;
ledger.migrate().await?;
let config = RunConfig::builder().action_ledger(Arc::new(ledger)).build();
```

## Unknown outcomes

A `NonIdempotent` call whose side effect may or may not have happened is answered with
an outcome-unknown function response instead of an error the model would read as an
invitation to try again:

```json
{ "status": "outcome_unknown", "detail": "Tool 'charge_card' timed out after 300 seconds. The call may have taken effect; check its result before calling it again." }
```

| Cause | Source |
|-------|--------|
| The call hit the agent's tool timeout | `LlmAgent` |
| The tool panicked | `LlmAgent` |
| The ledger holds a begun record for the key | `LlmAgent` |
| The run was cancelled while the call was in flight | `Runner` |

When a run is cancelled — `Runner::interrupt()` or a cancellation token — the runner
persists one function response for every call still in flight, so each call keeps exactly
one response and the next turn's model request shows the call and its unknown outcome.
`adk_core::is_outcome_unknown()` recognizes these responses.

## Delegation timeouts

`Tool::timeout_override()` replaces the agent's per-call `tool_timeout` for one tool:

| Value | Timeout applied to each call |
|-------|------------------------------|
| `None` (default) | The agent's `tool_timeout` |
| `Some(None)` | None; the call runs until it finishes or the run is cancelled |
| `Some(Some(duration))` | `duration` |

`AgentTool` returns its own `timeout` setting, so a delegation is bounded by the agent
tool's configuration rather than the parent's five-minute default:

```rust
use adk_tool::AgentTool;
use std::time::Duration;

// No per-call limit: the run's own budget and cancellation bound the delegation.
let researcher = AgentTool::new(research_agent.clone());

// A forty-five-minute limit for this delegation only.
let bounded = AgentTool::new(research_agent).timeout(Duration::from_secs(45 * 60));
```

## Related

- [Function Tools](function-tools.md) - Tool metadata attributes and builders
- [Retry & Reflect](retry-reflect.md) - Reflection prompts after tool failures
- [LlmAgent](../agents/llm-agent.md) - Tool resilience settings
- [Payments](../security/payments.md) - Commerce tools and scopes
