# Settings, While You Were Away, Memory and Record

The web home's **Today** and **Agent Memory and Rules** screens, and the same screens on a
phone or a terminal, read and change the [agent kernel](index.md) through four groups of
[control-plane](control-plane.md) routes (spec 3.3 §10 K3, K4, K5). All of them are under
`/api`, on the socket, the named pipe and HTTP.

| Group | Routes | Read | Change |
|---|---|---|---|
| Settings (K3) | `GET /kernel/settings[/{partition}]`, `POST /kernel/settings/{partition}/{quiet-time,spending-limit,mode}` | `operator`, or a device with `read` | The person, never a device token |
| While you were away (K4, feature `proc`) | `GET /kernel/away`, `POST /kernel/away/seen`, `GET /kernel/away/lines/{seq}` | `operator`, or a device with `read` | Its own cursor: `operator`, or a device with `read` |
| Memory (K5) | `GET /kernel/memory`, `POST /kernel/memory/facts/{id}/{confirm,fix,forget}`, `POST /kernel/memory/undo` | `operator`, or a device with `read` | The person, never a device token |
| Record (K5) | `GET /kernel/record`, `GET /kernel/record/check` | `operator`, or a device with `read` | — |

"The person" is a caller with the `operator` scope that no [device token](pairing.md)
admitted: the kernel's own socket or pipe, a loopback caller, or the home's owner signed in
with the server's bearer token or SSO. A chat channel reaches no kernel route. A change is
recorded as `kernel.control` before it takes effect; the memory records carry ids, never a
fact's text.

## Settings while the kernel runs

An area is a [partition](partitions.md), or `default` on a kernel without partitions.

| Setting | Route | Body | Started from |
|---|---|---|---|
| Quiet time | `POST /kernel/settings/{partition}/quiet-time` | `{"window": {"start": "22:00", "end": "07:00", "timeZone": "Europe/London"}}`, or `{"window": null}` for none. The times are read in the IANA `timeZone`, across daylight saving time; else at `utcOffsetMinutes` east of UTC, a fixed offset that moves the window by an hour when the clocks change; else in the kernel host's time zone | `KernelConfig::quiet_hours` (`[kernel] quiet_hours`, the first-run answer) and the partition record's own |
| Daily spending limit | `POST /kernel/settings/{partition}/spending-limit` | `{"microUsd": 5000000}`, or `{"microUsd": null}` for none | The lower of `KernelConfig::quotas` and the partition record's |
| Mode | `POST /kernel/settings/{partition}/mode` | `{"mode": "ask"}`: `read_only`, `plan`, `ask` or `auto` | The mode the area's harness started in (`ControlPlaneConfig::with_start_mode`), else the partition record's |

A change replaces, for its area, the value the kernel and the partition record started with,
while the area still starts with that value: a different start value — `adk-rust up --mode`,
`adk-rust partitions set-mode` followed by a restart, or quiet hours and a spend cap set at
start — is newer, takes over, and the change is no longer listed. A quiet window a workspace
sets (`[kernel] quiet_hours` in its `.adk/harness.toml`) still holds beside it; the answer
lists it as `alsoQuiet`. Two changes of one area made at once both hold: the store changes
the area's record by compare-and-swap. Each change takes effect from the agents' next step:

| Setting | Takes effect |
|---|---|
| Quiet time | Admission reads it at every claim: no proactive process of the area starts inside it. Work held for quiet time is woken, so admission decides on it again: turning quiet time off releases it at once, and a new window holds it until its own end |
| Daily spending limit | Admission reads it at every claim: no background process of the area starts once the day's spend reaches it. Work held for the budget is woken, so a raised or removed limit releases it at once |
| Quiet time, for chat | A decision card for the area waits for the end of the changed quiet time ([chat channels](pairing.md#chat-channels)) |
| Mode | `adk-rust up` gives each harness a mode store that reads the area's mode. A stricter mode tightens every session at its next step, and holds the overnight tidy-up and the kernel's questions back. A looser mode is refused (`409`) while an agent of the area is working, so no turn changes its rules midway (spec 3.1 I14); it reaches the sessions with no mode of their own, never one whose mode the person set or the harness tightened itself (the loop guard, a follow-up's pin) |
| Go ahead | Refused (`400`) on a kernel that refuses it at start: `adk-rust up` beyond loopback without `--sandbox` (`ControlPlaneConfig::with_refused_mode`); a Go ahead stored before is not applied there |

Admission keeps the quiet time and limit it read last when the control store cannot be read,
rather than falling back to the start values.

`GET /kernel/settings` answers every area:

```json
{
  "areas": [
    {
      "partition": "default",
      "primary": true,
      "quietTime": [{ "start": "23:00", "end": "06:00" }],
      "dailySpendingLimit": 5000000,
      "spentToday": 1840000,
      "mode": "ask",
      "changed": {
        "partition": "default",
        "quietTime": { "value": { "start": "23:00", "end": "06:00" }, "at": "2026-10-24T18:02:11Z", "by": "ana" }
      }
    }
  ]
}
```

`primary` marks the area the primary agent works in: the partition the kernel's own target
(`KernelConfig::app_name`) is bound to — under `adk-rust up`, the coordinator's partition, or
the first one — else `default`. The turns a terminal or the home sends run there, so both show
that area's spend today against its daily spending limit. In process, `Kernel::area_settings`
reads one area as `GET /kernel/settings/{partition}` answers it.

Changes live in the control plane's `ControlStore` (`ControlPlaneConfig::with_control_store`),
which `adk-rust up` keeps beside its other stores, so they survive a restart:

| Backend | Type | Feature |
|---|---|---|
| In-memory (the default) | `InMemoryControlStore` | — |
| SQLite, on `harness.db` | `SqliteControlStore` | `sqlite` |
| PostgreSQL | `PostgresControlStore` | `postgres` |
| Redis | `RedisControlStore` | `redis` |

`control_store_conformance_tests!` (feature `conformance`) checks a backend.

## While you were away

`GET /kernel/away` reads the [`/proc` ledger](proc.md) after the viewer's cursor and answers
what Today shows:

| Field | Meaning |
|---|---|
| `viewer` | `device:<id>` for a paired device, else the `viewer` query (default `home`) |
| `since` | When the viewer last marked the digest seen; a new viewer reads the last day |
| `after`, `through` | The page covers the ledger rows after `after` up to `through` |
| `more` | Rows after `through` wait for the next page |
| `counts` | `done`, `failed`, `stopped`, `suggestions`, `questions`, `decided` |
| `spentMicroUsd` | The spend rows of the page, summed |
| `quietTime` | `respected` unless a suggestion's work started inside a quiet window of its area, judged by the quiet time in force when it started (`breaches` lists the rows) |
| `waiting` | Decisions waiting for the caller now |
| `lines` | One line per event, each with its `record`: `GET /api/kernel/away/lines/{seq}` answers its ledger row; a row that makes no line is `404`. A line names its agent as the agent calls itself (`finance`, not `urn:adk:agent:adk-rust/finance`); the kernel's own rows say `kernel` |

| Line | Ledger row |
|---|---|
| `done`, `failed`, `stopped` | `process` of an agent ending `completed`, `failed` or `cancelled` |
| `suggestion` | `proposal` with effect `proposed` |
| `question` | `observation` |
| `decided` | `approval` or `rejection`, the audit record of a person's answer to a held call; `decided` |
| `failed` | `loop_guard` |
| `stoppedAll`, `resumedAll` | `governance`, `frozen` or `unfrozen` |

Nothing moves until the viewer marks the page seen with `POST /kernel/away/seen` and the
page's `through`. The cursor is a ledger sequence number in the control store and never moves
back, so marking `through` and reading again skips no row and repeats none, across restarts
of the kernel: a page read but not marked is read again in full. A page stops before a
missing sequence number younger than five seconds, which a store that numbers rows before
it commits them may still fill. `?after=` reads on without marking; `?limit=` caps the rows
(at most 2,000).

```bash
curl -s http://127.0.0.1:8080/api/kernel/away?viewer=terminal
curl -s -X POST http://127.0.0.1:8080/api/kernel/away/seen \
  -H 'content-type: application/json' -d '{"viewer": "terminal", "through": 120}'
```

## What your agents remember

`GET /kernel/memory?area=` lists the caller's live facts and the overnight tidy-ups:

| `origin` | Home's words | Harness fact |
|---|---|---|
| `youSaidThis` | You said this | `Known`, from the person's own words |
| `youConfirmedThis` | You said this | `Known`, confirmed or corrected by a person; it reads as the person's own words (spec 3.3 §8.1) |
| `guess` | A guess | `Assumed`; `noticedBy` names the agent that noticed it: the agent whose session it was learned in, as the session's first run stamped it |

| Route | Home's action | Effect |
|---|---|---|
| `POST /kernel/memory/facts/{id}/confirm` | That's right | The guess becomes "You said this" |
| `POST /kernel/memory/facts/{id}/fix` | Fix it | Body `{"text": "..."}`: a confirmed fact replaces it; the old one stays in the history |
| `POST /kernel/memory/facts/{id}/forget` | Forget it | Never recalled from the next model call on |
| `POST /kernel/memory/undo` | Undo | Body `{}` undoes the latest finished tidy-up that changed something, `{"tidyUp": "<id>"}` a named one |

The kernel holds no facts: a `MemoryBook` (`ControlPlaneConfig::with_memory`) does. `adk-rust
up` builds one over each area's harness when long-term memory is on, so every correction goes
through the harness, which updates recall, its index and its own audit records.

## What your agents did

`GET /kernel/record?limit=` lists the latest actions on the kernel's signed audit log, newest
first, as structured fields (`action`, `kernelAction` for the kernel's own records such as
`approvals.decide` or `settings.mode`, `subject`, `agent` — for an answer, the agent whose call
it decided — `tool`, `decision` — for a standing rule's record, `allowed` or `denied` — `outcome`,
`how`, `channel` — a paired device's name for its answers — `surface`, `target`, `title` —
the item it acted on as the person knows it, such as "News digest" for an answer to an agent's
screen action or "Finance" for a suggested agent — and, for a call, `toolSource`, its tool's publisher, and `policyRef`, what decided
it, `confirmation` for a person's answer) a surface words as "You let Finance use send_email, from
Telegram". `GET /kernel/record/check` verifies the whole log against its signed checkpoints:

| Field | Meaning |
|---|---|
| `valid` | Every entry links to the one before it, every checkpoint is signed by the kernel's key and matches the entries it covers: no action was changed or deleted |
| `entries` | How many actions the log holds |
| `checkedAt` | When the check ran |
| `problem` | The first failure, when `valid` is false |
| `report` | The verifier's report, as `adk-rust audit verify --json` prints it |

The check is the one `adk-rust audit verify` makes: `adk-rust up` gives the control plane a
`RecordBook` (`ControlPlaneConfig::with_record`) over the kernel's log that verifies it with
the kernel's own verifier key, so `valid` and the report match `adk-rust audit verify` on an
export of the same log with that key.

## From code

```rust
use std::sync::Arc;

use adk_core::InMemoryRunStore;
use adk_kernel::control::{ControlPlaneConfig, SqliteControlStore};
use adk_kernel::{Kernel, KernelConfig, PartitionMode};

async fn kernel(pool: sqlx::SqlitePool) -> adk_kernel::Result<Kernel> {
    let store = SqliteControlStore::new(pool);
    store.migrate().await?;
    let control = ControlPlaneConfig::default()
        .with_control_store(Arc::new(store))
        .with_start_mode("default", PartitionMode::Ask);
    Kernel::builder()
        .config(KernelConfig::default().with_control(control))
        .run_store(Arc::new(InMemoryRunStore::new()))
        .build()
}
```

`ControlClient` has a typed method per route: `settings`, `area_settings`,
`set_quiet_time`, `set_spending_limit`, `set_area_mode`, `away`, `mark_away_seen`,
`away_line`, `memory`, `confirm_fact`, `fix_fact`, `forget_fact`, `undo_tidy_up`, `record`
and `check_record`.
