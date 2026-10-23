# Self-observation: `/proc`

The kernel keeps a content-free ledger of what its agents do and, from it, learns each
partition's routine. When a routine changes for long enough, the kernel offers one
observation, as a question in neutral language — never a diagnosis (spec 3.2 §7.10).

```bash
adk-rust proc                                   # the observations the kernel offered
adk-rust proc baselines                         # each partition's 28-day baselines
adk-rust proc ledger                            # the last day of the ledger
adk-rust proc ledger --export ledger.jsonl      # every row, for `cargo adk eval --ledger`
```

The work is split in three tiers:

| Tier | Where | What |
|---|---|---|
| Ledger and live aggregates | `adk-kernel`, feature `proc` (on by default) | The ledger; the per-process live view and the OTel GenAI histograms `ps` and `top` show ([overview](index.md#processes)) |
| Statistics | `adk-eval`'s `ledger` module, pure functions | Median and MAD baselines, the warm-up, the deviation test, the meaningful and cooldown rules; `cargo adk eval --ledger` runs them in CI ([ledger statistics](../evaluation/evaluation.md)) |
| Baselines and observations | `adk-kernel`, feature `proc-baselines` | The daily pass that calls the statistics and records observations; on in adk-cli and in the umbrella's `full` |

## The ledger

One append-only table. A row records what kind of thing happened, when, for how long and
to which hashed subject:

| Column | Holds |
|---|---|
| `ts` | When, to the millisecond, in the offset of the user's local time |
| `partition` | The partition the activity belongs to |
| `agent` | The acting agent's id, or `kernel` for the kernel's own work |
| `kind` | `intent`, `tool_call`, `approval`, `rejection`, `denial`, `held`, `decided`, `rule_hit`, `turn`, `loop_guard`, `process`, `wake`, `spend`, `governance`, `observation`, `audit` |
| `effect` | A label such as the tool's effect class (`read_only`) or the state a process entered |
| `durationMs` | How long a tool call or a process took |
| `subjectHash` | `hmac-sha256:<hex>` of what the activity was about |
| `meta` | At most eight counts and flags, such as a turn's `model_calls` and `cost_micro_usd` |
| `traceId` | The run's invocation |

A row never holds a payload, a prompt, a file's content or a secret (K-I19). The ledger
refuses a row whose agent, partition or effect is not a label, whose `meta` holds anything
but numbers and booleans, or whose kind is not a short lowercase name; its error names the
field, never the value. What an activity was about — a tool call's arguments, a process id,
a wake source — reaches the row only as an HMAC under a key kept in the kernel home
(`ledger.key`, owner-only), so an export cannot be matched to subjects without the home.

| Writer | Rows |
|---|---|
| The kernel, from its event bus | `process` (with the time the process ran), `wake`, `held`, `decided`, `rule_hit`, `spend`, `governance`, `observation` |
| The harness, from each run | `intent` when a run starts; `tool_call` when a call's response arrives, with the tool's effect class and the time it took |
| The harness, from its audit records | `turn` (from `harness.spend`: model calls, tool calls, tokens, cost), `loop_guard`, `approval`, `rejection`, `denial`, `held`, and `audit` for the rest |

The ledger lives beside the kernel's other stores: in `harness.db`, or in the `--store`
PostgreSQL or Redis backend replicas share. It never leaves the host unless you export it.

## Baselines and the daily pass

Once a local day has ended, the kernel's maintenance tick enqueues one `Maintenance` process,
the durable run `kernel.proc`, keyed by the day so replicas run it once. It reads the
partition's rows — the kernel's own work and earlier observations left out — and tests
each day since its last pass:

| Rule | Value |
|---|---|
| Dimensions | Per local day: the first and the last activity's time of day; per kind, the count of rows, the median duration and the sum of each `meta` count |
| Baseline | Median and MAD over the 28 days before the comparison windows, weekdays and weekends apart |
| Warm-up | 14 days with a value before a dimension can deviate |
| Deviation | `\|recent − baseline\| > max(2.5 × MAD, floor)` in both the 7- and the 14-day window; the floor is 45 minutes for the time-of-day dimensions |
| Meaningful | Two dimensions deviating, or one for ten days running |
| Cooldown | Seven days per dimension after an observation names it |
| Catch-up | Up to 42 days after the kernel was down |

`adk-rust proc baselines` shows the last pass; here, an excerpt after two weeks of late
finishes:

```text
default — tested through 2026-10-22
    DIMENSION                     DAYS        MEDIAN      MAD    LAST 7   LAST 14  STATE
    activity.first_minute         weekday      09:00      0.0     09:00     09:00  usual
    activity.last_minute          weekday      17:30      0.0     20:30     20:30  changed
    activity.last_minute          weekend      11:30      0.0     11:30     11:30  usual
    intent.count                  weekday          2      0.0         2         2  usual
    tool_call.count               weekday         12      0.0        24        24  changed
    tool_call.duration_ms         weekday      165.5      0.0     207.5     207.5  changed
```

## Observations

An observation names the dimensions that changed, which way and since when, and the
comparison windows — no value. Its question is worded by a model only after the
deterministic test found the change; the model sees the dimension names, directions and
windows, never a ledger row. The kernel keeps the model's wording only when it is one line
ending in a question with no diagnosing, judging or instructing word, and uses its own
template otherwise, or when the day's phrasing budget is spent (K-I20):

```text
$ adk-rust proc
2026-10-22  default  Your routine has changed over the last two weeks: your day's last activity has been later than usual, along with higher counts and longer durations for tool calls. Would you like to review what changed?
    activity.last_minute up since 2026-10-15 (8 days)
    tool_call.count up since 2026-10-15 (8 days)
    tool_call.duration_ms up since 2026-10-15 (8 days)
    worded by gemini-3.5-flash-lite; id obs-20261022-37a8eec1
```

Each observation is also a `KernelEvent::Observation` on the event bus and a
`kernel.observation` audit record with its id, partition, dimensions and windows.

## Exporting the ledger

```bash
adk-rust proc ledger --export ledger.jsonl
cargo adk eval --ledger ledger.jsonl
```

Each line is one row as camelCase JSON, the format `cargo adk eval --ledger` reads; a CI job
can fail on a cost or latency drift of a nightly run this way. Without `--export`,
`adk-rust proc ledger` lists the last 24 hours (`--since`, `--kind`, `--agent`,
`--partition`, `--limit` narrow it):

```text
TIME                        PARTITION   KIND          EFFECT                  MS  AGENT
2026-10-22 11:45:00 +03:00  default     tool_call     read_only              162  urn:adk:agent:adk-rust/harness
2026-10-22 12:13:00 +03:00  default     tool_call     read_only              169  urn:adk:agent:adk-rust/harness
```

## Routes

| Route | Answers |
|---|---|
| `GET /api/kernel/proc/ledger` | A page of rows, `{entries, next}`; `?format=jsonl` answers one row per line with the next cursor in `x-ledger-next`. Filters: `partition`, `kind` (comma-separated), `agent`, `from`, `to`, `after`, `limit` (500, at most 5 000) |
| `GET /api/kernel/proc/baselines` | Each partition's last pass: `lastDay`, the drift state and the baselines |
| `GET /api/kernel/proc/observations` | The observations, newest day first: `partition`, `since`, `limit` |

All three need the `operator` scope, which the kernel's socket and a loopback bind grant. Each
read of the ledger is recorded as `kernel.control` with action `proc.ledger`.

## Using it from code

```toml
[dependencies]
adk-kernel = { version = "3.0.0", features = ["proc-baselines"] }
```

```rust
use std::sync::Arc;

use adk_core::{InMemoryRunStore, Llm};
use adk_kernel::proc::baselines::{BaselinePolicy, LlmPhraser};
use adk_kernel::proc::{Ledger, LedgerKey, SqliteLedgerStore};
use adk_kernel::{Kernel, KernelConfig};

async fn kernel(
    home: &std::path::Path,
    pool: sqlx::SqlitePool,
    model: Arc<dyn Llm>,
) -> adk_kernel::Result<Kernel> {
    let store = SqliteLedgerStore::new(pool);
    store.migrate().await?;
    let ledger = Ledger::new(Arc::new(store), LedgerKey::load_or_create(&home.join(LedgerKey::FILE))?);
    Kernel::builder()
        .config(KernelConfig::default().with_home(home))
        .run_store(Arc::new(InMemoryRunStore::new()))
        .ledger(ledger)
        .baseline_policy(BaselinePolicy::default().with_floor("tool_call.count", 3.0))
        .observation_phraser(Arc::new(LlmPhraser::new(model)))
        .build()
}
```

`BaselinePass` runs the same pass without a kernel, and `ledger_store_conformance_tests!`
(feature `conformance`) checks a new `LedgerStore` backend.

| Feature of `adk-kernel` | Default | Adds |
|---|---|---|
| `proc` | on | The ledger, its stores, the `/proc` routes, the OTel GenAI histograms |
| `proc-baselines` | off | The daily pass and observations over `adk-eval`'s statistics; the only `adk-kernel` → `adk-eval` edge |

## Read next

- [Overview](index.md): what `adk-rust up` runs
- [Kernel commands](cli.md): `adk-rust proc` beside `ps`, `top` and `events`
- [The control plane](control-plane.md): transports, authentication and audit records
