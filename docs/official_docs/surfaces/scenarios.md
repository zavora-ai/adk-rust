# Scenarios

Each phase of agent surfaces ships when its scenario passes end to end on SQLite and on one
shared store (spec 3.3 §12). The scenarios are programs you can run and watch: each starts the
`adk-rust up` binary as a real child process in a private home, plays one night, prints what
happened in plain words, and exits non-zero when any pass condition fails. Every pass condition
is read from the kernel's stores and routes, never from what a model wrote.

| Phase | Scenario | Example |
|---|---|---|
| 1. Decisions reach you | **Approve it tomorrow** | [`examples/surface_approve_tomorrow`](../../../examples/surface_approve_tomorrow/) |
| 2. While you were away | **The morning after** | [`examples/surface_morning_after`](../../../examples/surface_morning_after/) |
| Developer track | **Approve, rewind, fork** | `adk-surface/tests/workflow_scenario_tests.rs`, and `scripted_workflow` in the browser |

## Running them

Both run the `adk-rust` binary with the scripted model, the terminal and the PostgreSQL store
compiled in. Build it once from the repository root:

```bash
cargo build -p adk-cli --bin adk-rust --features scripted-model,tui,postgres
```

| Run | Command |
|---|---|
| SQLite, scripted model (no key) | `cargo run --manifest-path examples/surface_approve_tomorrow/Cargo.toml` |
| PostgreSQL, scripted model | `DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres cargo run --manifest-path examples/surface_approve_tomorrow/Cargo.toml -- --store postgres` |
| Live, `gemini-3.5-flash-lite` | `cargo run --manifest-path examples/surface_approve_tomorrow/Cargo.toml -- --live` |
| Live, `claude-sonnet-5-5` | `cargo run --manifest-path examples/surface_approve_tomorrow/Cargo.toml -- --provider anthropic` |
| The same for phase 2 | `cargo run --manifest-path examples/surface_morning_after/Cargo.toml [-- --store postgres \| --live \| --provider anthropic]` |

`--store postgres` creates a database for the run on the server `DATABASE_URL` names and drops
it afterwards. `--live` reads `GOOGLE_API_KEY` or `GEMINI_API_KEY` from the repository's `.env`, and
`--provider anthropic` reads `ANTHROPIC_API_KEY`.
`--bin PATH` runs another `adk-rust` binary; `--keep` keeps the run's folder and database.

> **Note:** `adk-rust up` runs on the system clock, so each night is compressed: quiet time is
> set with seconds (`[kernel] quiet_hours = "HH:MM:SS-HH:MM:SS"`) to a window a little ahead,
> routines fire inside it (`adk-rust wake --cron` with seconds), and each run takes about two
> minutes. Every transcript line names the time of the story it stands for and the real time.

## Approve it tomorrow

Quiet time 22:00–07:00. At 18:00 a routine prepares to email an invoice and waits for a
decision. Nobody answers; the kernel is killed with `kill -9` and started again. At 07:30 the
person taps **Allow once** in the Telegram thread. A stranger who writes to the bot gets a
pairing code and starts nothing.

| Pass condition | Read from |
|---|---|
| Nothing runs before the tap, and the email is sent exactly once | The mail driver's outbox |
| Nothing notifies during quiet time | Every message the Telegram adapter sent, with its time, from a fake Bot API on loopback |
| The web home shows "You said yes in Telegram" | `GET /api/surface/decisions/{id}` |
| The terminal shows the same | The `decided` line of `adk-rust --output json` |
| The record names the channel | `GET /api/kernel/record`: how `chat`, channel `Telegram` |
| A stranger gets a pairing code and starts nothing | The bot's reply, the process table and the inbox |

The phone's push notification arrives with the phone (spec 3.3 §5.4): the scenario checks
every notifier `adk-rust up` wires today, and its `phone-push` feature is where the push
receiver joins that check.

## The morning after

Over one night two routines finish, one fails, an agent suggests a payment, the person allows
a call from their phone, and the kernel restarts.

| Pass condition | Read from |
|---|---|
| One briefing when quiet time ends | `GET /api/kernel/away` with the phone's device token; once marked seen it does not come back, across another restart |
| Its counts match exactly across the restart | The process table, the agenda and the inbox |
| Its spending matches exactly across the restart | The kernel's daily spend total (`GET /api/kernel/settings/default`) |
| Every line links to its record | `GET /api/kernel/away/lines/{seq}` for each line |

## One verb, three origins

Ana snoozes News digest by typing `/snooze News digest` in the terminal, by clicking Snooze in
the home and by saying "Snooze news digest"; then Finance, an agent she added, asks to snooze
it (spec 3.3 §12, Experimental). The legs run as tests on an in-process kernel: the person's in
`adk-surface/tests/verbs_scenario_tests.rs`, Finance's in `adk-surface/tests/verbs_tests.rs`, and
`adk-rust up` on SQLite through a restart in `adk-cli/tests/verbs_added_agent.rs`.

```bash
cargo nextest run -p adk-surface --features verb-ui,web,tui,voice --test verbs_scenario_tests --test verbs_tests
cargo nextest run -p adk-cli --features verb-ui,scripted-model --test verbs_added_agent
```

| Pass condition | Read from |
|---|---|
| Each origin moves the wake by an hour and writes one `surface.verb` record and one `tool_call` allowed under `urn:adk:agent:adk-rust/surface.verbs` | The agenda, the audit log |
| The three records are equal but for where and how; the voice record carries the transcript's digest and never the words | The audit log |
| Finance's request waits in Go ahead with a matching standing rule: one inbox entry, origin `verbs:adk-rust`, low risk, never **Always allow** on any surface | The inbox, `GET /api/surface/decisions?surface=…` |
| The card waits through a restart; Allow once on the phone moves the item exactly once, and a second answer reads "already decided" | The agenda, the inbox, the record |
| "/snooze" typed in the home's message box or written by an agent, and the model's own audio, change nothing | The agenda, the audit log |
| While everything is stopped, Ana's snooze runs and Finance asks nothing | The agenda, the inbox |

## Approve, rewind, fork

The graph agent `invoice-run` drafts an invoice with a model step, charges it with a governed
HTTP POST that asks first and spends money, and files a receipt. The charge goes to a loopback
provider that counts every charge it receives. The scenario runs in-process on memory and on
SQLite, where the runner, the engine and the routes are rebuilt over the same files halfway
through; the PostgreSQL leg runs by hand.

| Run | Command |
|---|---|
| Memory and SQLite | `cargo nextest run -p adk-surface --features web,workflow --test workflow_scenario_tests` |
| PostgreSQL | `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo nextest run -p adk-surface --features web,workflow --test workflow_scenario_tests --run-ignored only` |
| In the browser | `cargo run -p adk-surface --features web,workflow --example scripted_workflow -- 8795 /tmp/workflow`, then `npx playwright test --project workflow` in `adk-surface/web` |

| Step | Pass condition | Read from |
|---|---|---|
| Start | One card for `pay`, with the full POST, money, **Confirm it's you and approve · Edit · Deny**, no **Always allow** and "From the invoice-run workflow · Open in Runs"; `draft` done, `pay` waiting with that decision; no charge | `GET /api/surface/decisions`, `GET /api/surface/workflow/sessions/{session}/run`, the provider's count |
| Approve from the Runs tab | The run is done; one charge; `surface.decide` recorded with surface `web`; one completed action-ledger record under `graph/<agent id>/<thread>/pay/1/<digest>`; the charge's writes in the effect log | The run view, the provider, the audit records, the ledger, `adk_graph::governed::effect_log` |
| Restart (SQLite) | The run view is the same | The run view, rebuilt over the same files |
| Rewind | The step before the charge holds no charge; the thread's checkpoints are unchanged | `GET …/checkpoints/{id}`, the checkpointer |
| Fork, call unchanged | Done with no decision; `pay` "Not sent again" with the original charge; still one charge; the lineage names the thread, the checkpoint and `note`; `surface.workflow.fork` recorded with surface `web` | The fork's run view and checkpoints, the provider, the audit records |
| Fork, call changed | A new decision for 5000; denied, `pay` not run and the run failed; a sibling, approved, charges once, and the next sibling is answered from the effect log | The fork's run view, the provider |
| Stop all | A fork is made and not started; started after **Resume all**, it is answered from the record; a freeze before a changed call's charge leaves no ledger record and no charge | The fork's run view, the ledger, the provider |

The original thread's checkpoints never change. The provider counts one charge per distinct
approved call: the 4200 charge once and the 5000 charge once.

## Related

- [Screen Actions](verbs.md) — Snooze in Coming up
- [The Engine](index.md) — decision cards, what each surface may decide, the answer log
- [The Workflow Observer](workflow.md) — the Runs tab, forks and `adk-rust graph`
- [The Terminal](terminal.md) — JSON lines and exit codes
- [Chat](chat.md) — the Telegram adapter
- [Today and Memory](../kernel/today-and-memory.md) — "While you were away" and the record
