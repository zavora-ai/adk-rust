# Model Fallback

`FallbackLlm` wraps an ordered list of models of any provider. Each request goes to the first
model; when that model fails on a configured trigger, the same request goes to the next one. The
first model that answers keeps the turn. `FallbackLlm` implements `adk_core::Llm`, so an agent
takes it wherever it takes any other model.

It is the provider-neutral counterpart of Google ADK's `FallbackModel` and of Claude's
refusal fallback, and it runs under the same run budget, spend ledger and kill switch as every
other model call.

## Installation

`FallbackLlm` is in `adk-model` and needs no feature of its own. Enable the features of the
providers in the chain:

```toml
[dependencies]
adk-agent = "3.0.0"
adk-model = { version = "3.0.0", features = ["anthropic", "gemini"] }
```

## Usage

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_model::{FallbackLlm, FallbackTrigger, GeminiModel};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let claude = AnthropicClient::new(AnthropicConfig::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-sonnet-5-5",
    ))?;
    let gemini = GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?;

    let model = FallbackLlm::builder(Arc::new(claude))
        .fallback(Arc::new(gemini))
        .fall_back_on(FallbackTrigger::Refusal)
        .build();

    let _agent = LlmAgentBuilder::new("assistant")
        .instruction("Answer briefly.")
        .model(Arc::new(model))
        .build()?;
    Ok(())
}
```

Each attempt is sent with `LlmRequest::model` set to that model's name, so a delegate never
receives the first model's name.

## Triggers

| Trigger | Default | Moves on when |
|---------|---------|---------------|
| `Retryable` | on | The model reports a rate limit, a timeout, a server error or an outage (`AdkError::is_retryable`) |
| `ModelNotFound` | on | The provider does not serve the model: an unknown, retired or unavailable model id (`ErrorCategory::NotFound`) |
| `QuotaExhausted` | on | The provider account's quota, credit balance or spend limit is used up (`ErrorCategory::ResourceExhausted`) |
| `Refusal` | off | The model declines the request (`FinishReason::Refusal`) before any output |
| `ContextOverflow` | off | The request is longer than the model's context window |
| `AuthFailure` | off | The provider refuses the credentials or the permission (`Unauthorized`, `Forbidden`) |

| Builder method | Effect |
|----------------|--------|
| `FallbackLlm::builder(primary)` | Starts a chain with the default triggers |
| `fallback(model)` | Appends a model after those already in the chain |
| `fall_back_on(trigger)` | Adds a trigger |
| `never_fall_back_on(trigger)` | Removes a trigger |
| `triggers(iter)` | Replaces the trigger set |

Only an error the model reports is a trigger. A refusal by the run's budget, its spend ledger or
the kill switch ends the chain with that error, and so does any error whose trigger is off. A
model's own retries (`RetryConfig`) run first; the chain sees a failure once they are used up and
tries each model once.

When every model fails, the last model's error is returned, or its refusal when the last model
declined. `LlmAgent` handles it as any model error: auto-compaction resends a request that was too
long, and an `on_model_error` plugin hook can still answer the turn.

## Streaming

A model that has yielded a response owns the turn. The chain moves on only when an attempt fails
before its first response — an error from the call or as the stream's first item, or a refusal
as its first response. After that, an error or a refusal reaches the caller, which already holds
the partial output.

| The attempt | Non-streaming | Streaming |
|-------------|---------------|-----------|
| Fails before responding | Next model | Next model |
| Refuses before any output (Anthropic's classifier refusals) | Next model, with `Refusal` | Next model, with `Refusal` |
| Refuses after streaming some text | — | The refusal reaches the caller |
| Fails mid-stream | — | The error reaches the caller |

## Budgets, the spend ledger and the kill switch

Under a run (`LlmAgent`, `CodeActAgent`, compaction, MCP sampling), every attempt is metered on
its own through `Llm::generate_content_metered`:

| Per attempt | Recorded |
|-------------|----------|
| Run budget | One model call, and the attempt's tokens and cost |
| Spend ledger | A hold reserved under the vendor of the attempt's model, committed under the provider its response reports; released when the attempt fails before responding |
| Kill switch | Checked before the attempt is sent, so a freeze stops the chain at its next model |

A refused attempt is billed by its provider and committed under it. A run that falls back from a
refusal on one provider to an answer on another shows two ledger entries:

| Attempt | Outcome | Ledger key | Committed |
|---------|---------|------------|-----------|
| `gpt-6.1-sol` | Refusal, billed | `org=acme agent=urn:adk:agent:acme/researcher vendor=openai` | 1,000 micro-USD |
| `gemini-3.7-flash` | 503, released | `vendor=gemini` | 0 |
| `claude-haiku-5-5` | Answer | `vendor=anthropic` | 2,000 micro-USD |

Wrap the chain last: a wrapper around a `FallbackLlm` that does not forward
`generate_content_metered` meters the whole chain as one call.

## Seeing which model answered

| Signal | Where |
|--------|-------|
| The model and provider that answered | `LlmResponse::model` and `LlmResponse::provider`, set by the answering provider |
| That a fallback happened | `provider_metadata["fallback"]` on every complete response, copied onto the agent's event |
| Each skipped model | A `warn` tracing event, `model failed; trying the next model in the fallback chain`, with `fallback.from`, `fallback.reason` and `error.code` |

```json
{
  "fallback": {
    "model": "claude-haiku-5-5",
    "position": 1,
    "skipped": [
      { "model": "claude-sonnet-5-5", "reason": "retryable", "error_code": "model.anthropic.api_error" }
    ]
  }
}
```

A response from the first model carries no marker. `position` counts from 0, the first model.

## Mixed chains

The chain is built once, before any attempt, so it reports what every model in it can serve:

| `Llm` method | Reports |
|--------------|---------|
| `name` | The first model's name |
| `context_window` | The smallest window among the models that report one, so auto-compaction keeps a request within every model it may reach; with `ContextOverflow`, the largest |
| `schema_adapter` | The first model's; tool schemas are normalized once, so keep the providers schema-compatible or put the strictest first |
| `max_cost_micro_usd` | The largest bound (a failed call bills nothing), or their sum with `Refusal`; `None` when a model has no bound |
| `native_web_search` | The search every model serves, or `None` when they differ |
| `uses_interactions_api` | `true` when any model uses it |

Each request starts again at the first model. Turns a fallback model produced carry its
provider origin, so native items such as thinking signatures are replayed only to that provider.

## Other fallbacks

| Mechanism | Scope | Page |
|-----------|-------|------|
| `FallbackLlm` | Any providers, any trigger above, client side | This page |
| Anthropic server-side `fallbacks` | Claude refusals, inside one Messages request | [Anthropic client](anthropic.md) |
| OpenRouter routing `models` | Models behind OpenRouter | [Model providers](providers.md) |
| `on_model_error` plugin hook | Any failure the chain does not absorb; its response answers the turn | [Plugins](../core/plugins.md#model-fallback) |

Anthropic's server-side fallback and `FallbackLlm` compose: a client with server-side `fallbacks`
can be one model of the chain.
