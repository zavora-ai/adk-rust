# Spend Ledger

A `SpendLedger` holds one budget for everything an agent spends: model calls and payments
draw on the same limits. Set it on `RunConfig::spend_ledger` and every model call reserves its upper-bound cost
before it is sent, while the payment tools reserve each checkout before it completes.

## Reserve, commit, release

| Step | Method | Effect |
|------|--------|--------|
| Before spending | `reserve(&key, amount)` | Holds the amount against every limit that covers the key, or fails with `spend.limit_exceeded` |
| After spending | `commit(id, actual)` | Records the actual amount and drops the hold |
| When nothing was spent | `release(id)` | Drops the hold |

Amounts are integer micro-USD (`1_000_000` is one US dollar). A hold that is neither
committed nor released stops counting once its time to live passes (15 minutes by
default), so a process that dies mid-call does not lock budget. A commit is recorded even
when it exceeds the reservation or a limit: the money was already spent.

## Keys and limits

A `SpendKey` names an organization and, optionally, one agent and one vendor. A reservation
attributes spend to its key; a limit caps every entry its key covers within a period.

| Period | Window |
|--------|--------|
| `SpendPeriod::Day` | UTC calendar day |
| `SpendPeriod::Month` | UTC calendar month |
| `SpendPeriod::Lifetime` | All recorded spend |

```rust
use adk_core::{InMemorySpendLedger, SpendKey, SpendLimits, SpendPeriod};

let ledger = InMemorySpendLedger::new(
    SpendLimits::new()
        // 50 USD per day for the whole app
        .limit(SpendKey::org("support-app").per(SpendPeriod::Day), 50_000_000)
        // 200 USD per month on Gemini
        .limit(
            SpendKey::org("support-app").with_vendor("gemini").per(SpendPeriod::Month),
            200_000_000,
        )
        // 5 USD per day for one agent
        .limit(
            SpendKey::org("support-app").with_agent("triage").per(SpendPeriod::Day),
            5_000_000,
        ),
);
```

Every limit that covers a reservation is checked, so the app, vendor, and agent caps above
apply together.

## Backends

| Ledger | Crate and feature | Concurrency |
|--------|-------------------|-------------|
| `InMemorySpendLedger` | `adk-core` | One mutex; process-local, nothing survives a restart |
| `SqliteSpendLedger` | `adk-session`, `sqlite` | `BEGIN IMMEDIATE` takes the write lock before the limit check |
| `PostgresSpendLedger` | `adk-session`, `postgres` | `SELECT ... FOR UPDATE` on the organization's row |
| `RedisSpendLedger` | `adk-session`, `redis` | One Lua script per reservation |
| `MongoSpendLedger` | `adk-session`, `mongodb` | Compare-and-set on the organization document |
| `Neo4jSpendLedger` | `adk-session`, `neo4j` | Transaction that writes the organization node first |
| `FirestoreSpendLedger` | `adk-session`, `firestore` | Serializable transaction over the organization's documents |

Concurrent reservations never overshoot a cap; every backend passes the same conformance
suite (see [Durable Stores](durable-stores.md)). Limits are configuration, not data: give
every process that shares a database the same `SpendLimits`. `with_clock` replaces the
system clock, which tests use to cross a day or month boundary.

```rust
use adk_core::{SpendKey, SpendLimits, SpendPeriod};
use adk_session::SqliteSpendLedger;

let ledger = SqliteSpendLedger::new("sqlite://spend.db?mode=rwc")
    .await?
    .with_limits(SpendLimits::new().limit(SpendKey::org("support-app").per(SpendPeriod::Day), 50_000_000));
ledger.migrate().await?;
```

## Model spend

With a ledger on the run config, every model call made on behalf of the run reserves before
it is sent and commits once it completes, through `adk_core::ModelCallScope`. The key is the
runner's app name, the calling agent, and the vendor. The reservation precedes the response,
so it names the vendor read from the model id (`gemini-*` is `gemini`, `claude-*` is
`anthropic`, `gpt-*`, `o1`, `o3`, and `o4` are `openai`, `deepseek-*` is `deepseek`,
`grok-*` is `xai`, Mistral model families are `mistral`; unrecognized ids are `unknown`).
The cost is committed under the provider the response reports in `LlmResponse::provider` —
`bedrock` for a Claude model served by Amazon Bedrock, for example.

> **Note:** when a limit already refuses a zero-amount reservation under the reported
> provider, the cost is committed under the vendor read from the model id and a warning is
> logged. Organization and agent totals are correct either way.

### Hard caps

The reservation is an upper bound on the call's cost, `Llm::max_cost_micro_usd`, so the
committed cost never exceeds it and a cap is never overshot.

| Bound term | Value |
|------------|-------|
| Input tokens | UTF-8 byte length of every text and JSON part, tool declaration and response schema (no tokenizer emits more than one token per byte), inline media at least 16,384 tokens per part, plus 2,048 tokens of framing and 16 per message and per tool |
| Input rate | The highest of the uncached, cache-read and cache-write rates, at the long-context tier when the input bound reaches it |
| Output tokens | The request's output cap (`max_output_tokens`, reasoning and thinking included), or the model's published output limit when the request sets none |
| Output rate | The model's output rate at the same tier |
| Built-in tool fees | Per-response fees once each; per-use fees × each declared built-in tool's `max_uses` |

| Provider | Output tokens used for the bound |
|----------|----------------------------------|
| Gemini, OpenAI (Chat and Responses), OpenAI-compatible, Azure AI | Request cap, else the model's output limit |
| Anthropic | The `max_tokens` the client sends (request cap, `AnthropicConfig::max_tokens`, or the model default) |
| Groq, DeepSeek | The model's output limit; neither vendor states whether reasoning counts toward the request cap |
| Bedrock | Request cap only |
| Ollama (local), mistral.rs | Zero: local inference has no per-token price |
| OpenRouter, OpenCode, xAI without a request cap, unpriced models | No bound |

A call with no bound reserves `UNBOUNDED_HOLD_MICRO_USD`, which every cap refuses: it is
refused wherever a limit covers its key, and runs only where none does. It then commits its
reported cost, or the `LlmSpendEstimate` when it reports none. Set an output cap on the
agent (`LlmAgentBuilder::max_output_tokens`) to keep reservations small under a tight cap.

> **Important:** a custom `Llm` wrapper must forward `max_cost_micro_usd` to the model it
> wraps; without it the wrapper's calls are unbounded and refused under a cap.

```rust
use adk_core::{RunConfig, SpendKey, SpendLimits, SpendPeriod};
use adk_runner::{LlmSpendEstimate, Runner};
use adk_session::SqliteSpendLedger;
use std::sync::Arc;

let ledger = Arc::new(
    SqliteSpendLedger::new("sqlite://spend.db?mode=rwc")
        .await?
        .with_limits(SpendLimits::new().limit(SpendKey::org("support-app").per(SpendPeriod::Day), 50_000_000)),
);
ledger.migrate().await?;

let runner = Runner::builder()
    .app_name("support-app")
    .agent(agent)
    .session_service(sessions)
    .run_config(RunConfig::builder().spend_ledger(ledger).build())
    .build()?
    // Committed by calls to unpriced models whose responses report no cost.
    .with_llm_spend_estimate(LlmSpendEstimate::per_call(50_000));
```

| What the call returned | Ledger action |
|------------------------|---------------|
| A response reporting `usage_metadata.cost` | Commit that cost at the turn-complete response |
| Responses, but no cost | Commit the bound (or, for an unbounded call, the estimate) and log a warning |
| No response (the call failed) | Release |

The committed cost includes the published fees of provider-executed built-in tools — web
search, file search, Google Search grounding — the call used; see
[Built-in tool uses and fees](../models/providers.md#built-in-tool-uses-and-fees). The
bound covers them: per-response fees once each, per-use fees up to each declared built-in
tool's `max_uses`. A request declaring a fee-bearing built-in tool without `max_uses` has no
bound, so it is refused under a cap.

A refused reservation fails the model call with the ledger's `spend.limit_exceeded` error,
so the run stops before it spends past the cap.

### What is metered

Every model call made on behalf of a run reserves, commits and releases through the same
ledger and counts against the same [run budget](../core/runner.md#run-budgets).

| Model call | Metered through | Attributed to |
|------------|-----------------|---------------|
| `LlmAgent` and `CodeActAgent` turns | `ModelCallScope::for_run` | The calling agent |
| `LlmConditionalAgent` routing | `ModelCallScope::for_run` | The routing agent |
| Post-run compaction (`EventsCompactionConfig`) | `ModelCallScope`, made current around the summarizer | The agent whose context is compacted |
| Intra-invocation compaction (`IntraCompactionConfig`) | `ModelCallScope`, made current around the summarizer | The agent about to run |
| Token-limit compaction (`context-compaction` feature) | `ModelCallScope`, made current around the strategy | The agent about to run |
| MCP sampling (`mcp-sampling` feature) | `ModelCallScope` of the run whose tool call is in flight | The agent that called the MCP tool |

The built-in summarizers (`LlmEventSummarizer`, `SummarisationCompaction`) and
`LlmSamplingHandler` call their model through `adk_core::generate_metered`, which meters
against the scope the runner or the MCP client made current. A custom `BaseEventsSummarizer`,
`CompactionStrategy` or `SamplingHandler` is metered the same way when it calls its model
through `generate_metered` or wraps it in `adk_core::MeteredLlm`.

| Behaviour | Detail |
|-----------|--------|
| Refusal | A call the ledger or the budget refuses is never sent; a refused compaction is skipped with a warning and the run continues, a refused sampling request returns an error to the MCP server |
| Hold size | The call's upper bound; see [Hard caps](#hard-caps) |
| Sampling outside a run | Counts against the scope attached with `McpToolset::with_sampling_scope`, otherwise unmetered |
| Several runs on one MCP connection | A sampling request counts against the run whose tool call started most recently |

```rust
use adk_core::{InMemorySpendLedger, ModelCallScope};
use std::sync::Arc;

// Calls outside any run: meter them explicitly.
let scope = ModelCallScope::new()
    .with_spend_ledger(Arc::new(InMemorySpendLedger::default()), "support-app")
    .with_agent("nightly-digest");
let stream = scope.generate(model.as_ref(), request, false).await?;
```

Out of scope: evaluation judges, prompt optimizers and simulators in `adk-eval` run in the
evaluation harness, not on behalf of a run; wrap their model in `MeteredLlm` with an explicit
scope to meter them. `OpenAIResponsesClient::compact_response` is called by application
code, not by the runner. Realtime sessions (`adk-realtime`) are billed per session and audio
minute and do not go through the `Llm` trait.

## Payment spend

The payment tools reserve each checkout total at completion under the key
`app / agent / merchant_id`, commit it when the checkout completes, and release it when the
checkout fails. See [Payments and Commerce](payments.md#payment-policies).

## Error codes

| Code | Category | Meaning |
|------|----------|---------|
| `spend.limit_exceeded` | `Forbidden` | The reservation would exceed a limit; the details carry the limit key, cap, consumed, and requested amounts |
| `spend.ledger_unavailable` | `Unavailable` | The backing store failed; nothing was reserved |
| `spend.unknown_reservation` | `NotFound` | The reservation was never taken or was already settled |
| `spend.amount_too_large` | `InvalidInput` | The amount does not fit the ledger's storage |
