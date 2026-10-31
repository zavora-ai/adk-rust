# Scenarios

Each phase of agent surfaces ships when its scenario passes end to end on SQLite and on one
shared store (spec 3.3 §12). Every scenario below runs on SQLite, and on PostgreSQL in a
database of its own. Every pass condition is read from the kernel's stores and routes, or from
what left the machine, never from what a model wrote.

| Phase | Scenario | SQLite | PostgreSQL |
|---|---|---|---|
| 1. Decisions reach you | **Approve it tomorrow**, **A stranger writes** | [`examples/surface_approve_tomorrow`](../../../examples/surface_approve_tomorrow/) | `--store postgres` |
| 2. While you were away | **The morning after** | [`examples/surface_morning_after`](../../../examples/surface_morning_after/) | `--store postgres` |
| 3. Memory and rules | **Correct me, then check** | `adk-cli/tests/correct_me_then_check.rs` | `correct_me_then_check_on_postgres` |
| 4. Areas and roster | **Work stays at work** | `adk-harness/tests/work_stays_at_work.rs` | `work_stays_at_work_on_postgres` |
| 4. Areas and roster | **Hire a helper** | `adk-cli/tests/hire_a_helper.rs` | `hire_a_helper_on_postgres` |
| 5. Presence and voice | **Speak, glance, stop** | `adk-cli/tests/speak_glance_stop.rs` | `speak_glance_stop_on_postgres` |
| Developer track | **Approve, rewind, fork** | `adk-surface/tests/workflow_scenario_tests.rs`, and `scripted_workflow` in the browser | `approve_rewind_fork_on_postgres` |
| Developer track | **Governed form**, **Hostile app** | `adk-surface/tests/genui_scenario_tests.rs`, `genui_apps_tests.rs` | `a_drawn_form_is_sent_held_and_runs_once_on_postgres` |
| Experimental | **One verb, three origins** | `adk-surface/tests/verbs_scenario_tests.rs`; on `adk-rust up`, `adk-cli/tests/verbs_added_agent.rs` | `one_verb_three_origins_on_up_on_postgres` |

The PostgreSQL legs are `#[ignore]`d and run by hand against the server `DATABASE_URL` names,
each in a database of its own. Each section below, or the test's module docs, gives its
command.

## Running the examples

The phase 1 and 2 scenarios are programs you can run and watch: each starts the `adk-rust up`
binary as a real child process in a private home, plays one night, prints what happened in
plain words, and exits non-zero when any pass condition fails. Both run the `adk-rust` binary
with the scripted model, the terminal, the PostgreSQL store and the phone compiled in. Build it
once from the repository root:

```bash
cargo build -p adk-cli --bin adk-rust --features scripted-model,tui,postgres,mobile
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
| The phone's card shows the same | `GET /api/surface/decisions/{id}?surface=phone` with the phone's device token |
| Nothing wakes the phone during quiet time | Every Web Push wake-up, with its time, from a push service on loopback the phone subscribed to (`up --push-to-localhost`) |
| The terminal shows the same | The `decided` line of `adk-rust --output json` |
| The record names the channel | `GET /api/kernel/record`: how `chat`, channel `Telegram` |
| A stranger gets a pairing code and starts nothing | The bot's reply, the process table and the inbox |

## The morning after

Over one night two routines finish, one fails, an agent suggests a payment, the person allows
a call from their phone, and the kernel restarts.

| Pass condition | Read from |
|---|---|
| One briefing when quiet time ends | `GET /api/kernel/away` with the phone's device token; once marked seen it does not come back, across another restart |
| No briefing before quiet time ends | Just before the end the phone's page holds the night while the quiet time `GET /api/kernel/settings/default` serves still holds, so the home's rule for the morning briefing holds it back |
| Its counts match exactly across the restart | The process table, the agenda and the inbox |
| Its spending matches exactly across the restart, and is not zero | The kernel's daily spend total (`GET /api/kernel/settings/default`); each scripted reply names its price (`"cost"` in `--model-script`) |
| Every line links to its record | `GET /api/kernel/away/lines/{seq}` for each line |

## Work stays at work

Home asks first; Work goes ahead with a 09:00 weekday routine (spec 3.3 §12 phase 4). One run
of a kernel hosting both areas' harnesses on a scripted model and a manual clock.

| Store | Command |
|---|---|
| SQLite | `cargo nextest run -p adk-harness --features kernel,memory --test integration -E 'test(/^work_stays_at_work::/)'` |
| PostgreSQL | `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo nextest run -p adk-harness --features kernel,memory,postgres --test integration -E 'test(/^work_stays_at_work::/)' --run-ignored only` |

| Pass condition | Read from |
|---|---|
| A Work agent reading the Home calendar waits, even in Go ahead | The inbox, and what reaches Work |
| The routine runs on weekdays and skips Saturday and Sunday | The process table on a manual clock |
| Spending per agent sums to the areas' and the day's totals | The spend ledger |

## Hire a helper

In the terminal the person asks ADK-Rust for a Finance agent (spec 3.3 §12 phase 4, §5.7). The
test runs `adk-rust up` as a child process with a scripted model, killed with `kill -9` and
started again halfway through.

| Store | Command |
|---|---|
| SQLite | `cargo nextest run -p adk-cli --features scripted-model,audit-sql --test hire_a_helper` |
| PostgreSQL | `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo nextest run -p adk-cli --features scripted-model,audit-sql,postgres --test hire_a_helper --run-ignored only` |

| Pass condition | Read from |
|---|---|
| Before any answer nothing is added, and the chat card offers only **Don't add** | The roster, the process table, `GET /api/surface/decisions/{id}?surface=chat` |
| After **Add this agent**, Finance is **Ready** in the terminal's roster on the kernel event that announces it, and in the web home's, in one colour | The engine over the kernel's control plane, following `GET /api/kernel/events`; `GET /api/surface/agents` |
| After `kill -9` and a restart, Finance keeps its id, acts as `urn:adk:agent:adk-rust/finance`, and ADK-Rust refuses its thread | The roster, the record line of Finance's call, the refused run |
| Renamed "Bills" in the web home, the terminal shows it on the event that announces it; in Suggest its next turn changes nothing | The terminal's roster, the workspace, the inbox |
| A page asking for an agent with a shell on this computer is refused; Bills cannot suggest agents | `kernel.agent.refused`, the inbox |
| Removed in the terminal, its waiting decision ends without an answer and the phone's late answer changes nothing; at its routine's time nothing of Bills runs, while ADK-Rust's routine at the same time does; a new Finance is `finance-2` with no rule | The inbox, the process table, the rules |
| The record holds `proposed`, `created`, `changed`, `removing` and `removed` in order, each naming its surface, and `adk-rust audit verify` passes | `GET /api/kernel/record`, `adk-rust audit export` and `verify` |

## Speak, glance, stop

With work in flight, the person speaks, glances at the tray and stops everything (spec 3.3 §12
phase 5). `adk-rust up` serves the phone (`--home-url`, an operator token) with the voice pack,
a scripted model, a scripted realtime provider, and a press driver whose `publish_post` is
`publish_public`, so it is high risk.

| Store | Command |
|---|---|
| SQLite | `cargo nextest run -p adk-cli --features voice,mobile,scripted-model --test speak_glance_stop` |
| PostgreSQL | `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo nextest run -p adk-cli --features voice,mobile,scripted-model,postgres --test speak_glance_stop --run-ignored only` |
| The tray's shell on WebKitGTK, against `adk-rust up` on SQLite and PostgreSQL | `.github/workflows/desktop.yml` (`adk-surface/desktop/e2e.sh store_gate`), on Linux with a display |

| Pass condition | Read from |
|---|---|
| A spoken "allow once" runs an everyday call once, after the cancel window | The voice socket's updates, the call's side effect |
| A high-risk call is refused by voice and approved in the web home with **Confirm it's you** | `needsScreen`, the card's "High risk", a passkey answering `POST /api/surface/decisions/{id}/confirm`, the call's side effect |
| The tray changes within one event | The tray's status read after each frame of `GET /api/kernel/events`: the waiting call shows on its `held` event |
| Stop all from the tray stops a running turn, a voice call and a held call within about a second | The running turn's steps and run state, the voice socket's "All agents are stopped", the held call's side effect after it is allowed |

A turn already inside a tool call finishes that call and parks at its next model or tool
boundary (spec 3.2 §7.2, K-I8), so the running turn takes at most the step it was in.

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

On `adk-rust up` the person's three origins run again on SQLite and on PostgreSQL, and the
signed records `adk-rust audit export` reads back differ only in where and how each was given:

```bash
cargo nextest run -p adk-cli --features verb-ui,voice,scripted-model,audit-sql --test verbs_added_agent
```

| Store | Command |
|---|---|
| PostgreSQL | `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo nextest run -p adk-cli --features verb-ui,voice,scripted-model,audit-sql,postgres --test verbs_added_agent --run-ignored only` |

| Pass condition | Read from |
|---|---|
| Each origin moves the wake by an hour and writes one `surface.verb` record and one `tool_call` allowed under `urn:adk:agent:adk-rust/surface.verbs` | The agenda, the audit log |
| The three records are equal but for where and how; the voice record carries the transcript's digest and never the words | The audit log |
| Finance's request waits in Go ahead with a matching standing rule: one inbox entry, origin `verbs:adk-rust`, low risk, never **Always allow** on any surface | The inbox, `GET /api/surface/decisions?surface=…` |
| The card waits through a restart and reads the same in the terminal attached to the kernel; Allow once on the phone moves the item exactly once, and a second answer reads "already decided" | The agenda, the inbox, the record |
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

## Governed form

An agent draws a form, a person sends it, and the call waits for them however the agent is
configured (spec 3.3 §12 "Governed form", §7.4 rule 5). It runs as tests over the real
kernel, durable worker, approval service, session store and audit trail on SQLite, and on
PostgreSQL by hand, with a scripted model driving the harness agent
Finance in "Ask first".

| Run | Command |
|---|---|
| The scenario, the projection and the terminal over the control plane | `cargo nextest run -p adk-surface --features genui,web,tui --test genui_scenario_tests --test genui_projection_tests` |
| PostgreSQL, on an empty database | `createdb genui_form`, then `DATABASE_URL=postgres://postgres:postgres@localhost:5432/genui_form cargo nextest run -p adk-surface --features genui,web,tui --test genui_scenario_tests --run-ignored only -E 'test(/on_postgres/)'`, then `dropdb genui_form` |
| Another area's and an added agent's forms | `cargo nextest run -p adk-cli --features genui,scripted-model --test genui_up` |
| The web home, light and dark | `npx playwright test --project genui --project genui-states`, from `adk-surface/web` |
| Live, `claude-sonnet-5-5`: the model draws the form, and its next turn reads the call as the person's | `cargo test -p adk-surface --features genui,web --test genui_scenario_tests -- --ignored --nocapture` |
| Live, `gemini-3.5-flash-lite` | `GENUI_LIVE_PROVIDER=gemini GENUI_LIVE_MODEL=gemini-3.5-flash-lite cargo test -p adk-surface --features genui,web --test genui_scenario_tests -- --ignored --nocapture` |

The live runs read `ANTHROPIC_API_KEY`, or `GEMINI_API_KEY` and then `GOOGLE_API_KEY`, from the
repository's `.env` or the file `ADK_ENV_FILE` names; `GENUI_LIVE_MODEL` picks another model.

| Pass condition | Read from |
|---|---|
| One drawing by Finance reading "Sending runs send_invoice and asks you first"; a drawn "Allow once" button is a plain refused notice; the inbox is empty | The thread and the inbox |
| A send makes one waiting card for `send_invoice` with the typed `to` and `amount` and `invoice` INV-10 from the stored drawing; nothing runs | The card and the tool's count |
| Its timeline starts "You pressed Send on a form Finance drew", no surface offers **Always allow**, and the chat card carries no arguments | The cards on the web home, the phone, the terminal and chat |
| An old version (`surface.drawn_changed`), a forged constant, an undeclared action, another user's session (`404`) and a chat or agent origin (`surface.not_here`) are refused | `POST /api/surface/commands` and the inbox |
| Go ahead, a standing rule, a confirmation handler and a stored approval of the identical call answer nothing: it still waits | The inbox and the tool's count |
| After the engine and kernel are rebuilt over the same stores, the form says "Sent at 10:03 · waiting for you" | `GET /api/surface/genui/sessions/{session}/forms` |
| **Allow once** on the phone runs `send_invoice` exactly once in Finance's own target; a denied twin never runs and says "Sent · you said no" | The tool's count and the forms route |
| A rule that refuses the tool refuses a send before it is held, and again when a held one resumes | The thread |
| A form sent from the terminal over the control plane waits as one card | The inbox |
| A form drawn in the Home area and one drawn by the added agent Bookkeeper are each held and run in their own target; none is answered "not run" | The process table and each agent's workspace |
| `surface.drawn.submitted` and `surface.decide` each name their own surface and origin | The audit trail |

### Hostile app

An MCP server's app shown in a thread can never approve anything (spec 3.3 §12, §6.1
`genui-apps`). The Weather server is an in-process MCP server; the kernel, worker, harness,
approval service and sessions are real, on SQLite.

| Run | Command |
|---|---|
| Both legs | `cargo nextest run -p adk-surface --features genui-apps,tui --test genui_apps_tests` |
| The client side: the extension, `_meta.ui` and app resources | `cargo nextest run -p adk-tool --features mcp,mcp-apps --test mcp_apps_tests` |
| The web home's frame and bridge, light and dark | `npx playwright test --project genui-apps`, from `adk-surface/web` |

| Pass condition | Read from |
|---|---|
| `weather_show`'s HTML carries an "Approved ✓ Allow once" card: "This app was blocked: it shows approval buttons", no frame, a `surface.app.blocked` record | `GET /api/surface/genui/sessions/{session}/apps/{call}` and the audit trail |
| An innocent-looking app asks for `transfer_funds` (model-only) and `send_invoice` (another server's): both refused and recorded; an app-only tool is "This app needs a tool this home can't run yet" | The app's answers and the audit trail |
| A tool the app may call waits as the agent's card, "The app from Weather asked", never **Always allow**; nothing runs | The inbox and the tool's count |
| A decide `POST` from the apps origin answers `403` under default and development security, recorded as `surface.app.origin_refused`; the card's version is unchanged | `POST /api/surface/commands`, the audit trail and the card |
| `ui/message` "Approved" is only a suggestion; **Send it** sends it as the person's message, marked as the app's: the model reads it in an untrusted-content block, the thread and the queue show "Approved" and "Suggested by the Weather app" | The thread, the queue and the session |
| A CSP declaring `http://127.0.0.1` is blocked | The app's open answer |
| The frame is labelled "it can't approve anything", sandboxed with scripts only, and outside the decision background | The Playwright screenshots |

## Related

- [Screen Actions](verbs.md) — Snooze in Coming up
- [The Engine](index.md) — decision cards, what each surface may decide, the answer log
- [The Workflow Observer](workflow.md) — the Runs tab, forks and `adk-rust graph`
- [The Terminal](terminal.md) — JSON lines and exit codes
- [Chat](chat.md) — the Telegram adapter
- [Today and Memory](../kernel/today-and-memory.md) — "While you were away" and the record
- [Agent-Drawn UI](genui.md) — forms agents draw and the calls a person asks for through them
