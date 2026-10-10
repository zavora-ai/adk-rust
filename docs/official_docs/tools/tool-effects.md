# Tool Effects

Every tool declares what calling it does to the outside world. The runtime uses that
declaration to decide whether a failed call may be retried.

## Tool effects

`Tool::effect()` returns a `ToolEffect`:

| Effect | Meaning | Retried after a retryable error |
|--------|---------|---------------------------------|
| `ReadOnly` | Reads without changing anything | Yes |
| `Idempotent` | Changes state; repeating a call leaves the same state as making it once | Yes |
| `NonIdempotent` | Repeating a call repeats the change — a payment, an email, an order | No |

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

`ToolContext::idempotency_key()` identifies one tool call across every attempt. The
default is `"{app}/{user}/{session}/{invocation}/{function_call_id}"`. Pass it to any
downstream API that deduplicates requests on a key.

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
