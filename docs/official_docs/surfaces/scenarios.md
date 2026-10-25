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

## Related

- [The Engine](index.md) — decision cards, what each surface may decide, the answer log
- [The Terminal](terminal.md) — JSON lines and exit codes
- [Chat](chat.md) — the Telegram adapter
- [Today and Memory](../kernel/today-and-memory.md) — "While you were away" and the record
