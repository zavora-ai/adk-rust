# Spend Ledger

A `SpendLedger` holds one budget for everything an agent spends: model calls and payments
draw on the same limits. Set it on `RunConfig::spend_ledger` and the runner reserves before
each model call, while the payment tools reserve each checkout before it completes.

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

Concurrent reservations never overshoot a cap. Limits are configuration, not data: give
every process that shares a database the same `SpendLimits`.

```rust
use adk_core::{SpendKey, SpendLimits, SpendPeriod};
use adk_session::SqliteSpendLedger;

let ledger = SqliteSpendLedger::new("sqlite://spend.db?mode=rwc")
    .await?
    .with_limits(SpendLimits::new().limit(SpendKey::org("support-app").per(SpendPeriod::Day), 50_000_000));
ledger.migrate().await?;
```

## Model spend

With a ledger on the run config, the runner installs an `LlmSpendRecorder` ahead of every
other invocation hook. The key is the runner's app name, the calling agent, and the vendor.
The reservation precedes the response, so it names the vendor read from the model id
(`gemini-*` is `gemini`, `claude-*` is `anthropic`, `gpt-*`, `o1`, `o3`, and `o4` are
`openai`; unrecognized ids are `unknown`). The cost is committed under the provider the
response reports in `LlmResponse::provider` — `bedrock` for a Claude model served by
Amazon Bedrock, for example — and the model's later calls in the run reserve under that
provider.

> **Note:** when a limit already refuses a zero-amount reservation under the reported
> provider, the cost is committed under the vendor read from the model id and a warning is
> logged. Organization and agent totals are correct either way.

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
    // Hold 4,096 output tokens at 10 USD per million when the request caps output.
    .with_llm_spend_estimate(LlmSpendEstimate::per_call(50_000).with_output_price(10_000_000));
```

| What the recorder saw | Ledger action |
|-----------------------|---------------|
| A chunk reporting `usage_metadata.cost` | Commit that cost |
| Chunks, but no cost | Commit the estimate and log a warning |
| No chunk (the call failed or a later callback skipped it) | Release |

A refused reservation fails the model call with the ledger's `spend.limit_exceeded` error,
so the run stops before it spends past the cap. The overshoot is bounded by the difference
between one call's actual cost and its estimate.

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
