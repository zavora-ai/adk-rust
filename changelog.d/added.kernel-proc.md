- **`/proc`: the kernel's content-free activity ledger, baselines and observations**
  (`adk-kernel`, `adk-harness`, `adk-cli`; spec 3.2 §7.10, K10).
  - `adk_kernel::proc` (feature `proc`, on by default): one append-only ledger of
    `ts, partition, agent, kind, effect, duration_ms, subject_hash, meta, trace_id` rows.
    `Ledger` refuses anything but labels, counts and durations, and stores what an activity
    was about only as an HMAC under a per-kernel `LedgerKey` kept in the kernel home
    (`ledger.key`). `LedgerStore` on in-memory, SQLite, PostgreSQL and Redis
    (`InMemoryLedgerStore`, `SqliteLedgerStore`, `PostgresLedgerStore`, `RedisLedgerStore`)
    with `ledger_store_conformance_tests!`.
  - `KernelBuilder::ledger` writes a row for every `KernelEvent` but `Health` and hands the
    ledger to the hosted runtime through `ProgressSource::attach_ledger`; under its
    `kernel` feature the harness writes each run's `intent` and tool calls (effect class,
    duration, hashed fingerprint) and its audit records (`turn` from `harness.spend`,
    `loop_guard`, `approval`, `rejection`, `denial`, `held`) through `LedgerAuditTap`.
  - `GET /api/kernel/proc/ledger` (operator only, paged by `seq`, `?format=jsonl` writes the
    JSONL `cargo adk eval --ledger` reads, each read recorded as `kernel.control`),
    `/api/kernel/proc/baselines` and `/api/kernel/proc/observations`, with
    `ControlClient::{ledger, baselines, observations}`.
  - Feature `proc-baselines` (off in `adk-kernel`, on in adk-cli and the umbrella's `full`):
    the maintenance tick enqueues one `kernel.proc` `Maintenance` process per local day,
    which runs `adk-eval`'s ledger statistics per partition — 28-day baselines, the 14-day
    warm-up, the 7- and 14-day deviation test, two dimensions or one for ten days, a 7-day
    cooldown — and records each `Observation` as a neutral question: worded by a model
    (`LlmPhraser`, within `BaselinePolicy::daily_budget`) only after the deterministic
    detection, else by the kernel's template; `KernelEvent::Observation` and the
    `kernel.observation` audit record name its dimensions and windows, never a value.
  - `adk-rust proc [ledger|baselines|observations]`, with `proc ledger --export FILE`;
    `adk-rust up` keeps the ledger beside its other stores and words observations with the
    harness's model.
