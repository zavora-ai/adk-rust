# surface_approve_tomorrow

"Approve it tomorrow", the phase 1 scenario of spec 3.3 §12 ("Decisions reach you"), run as a
program you can watch.

Quiet time 22:00–07:00. At 18:00 a routine prepares to email an invoice and waits for a
decision. Nobody answers; the kernel is killed with `kill -9` and started again. At 07:30 the
person taps **Allow once** in the Telegram thread. A stranger who writes to the bot gets a
pairing code and starts nothing.

The kernel is the `adk-rust up` binary, a real child process, with the Telegram adapter on.
Telegram is a fake Bot API on loopback that this program serves and that outlives the
kernel's restart, as Telegram's servers do; with it the program sees every message that would
leave the machine, and when. The kernel serves the phone (`--home-url http://localhost:<port>`
with an operator token): Ana's phone is paired, and its browser subscribes to Web Push through
a push service on loopback this program also serves (`--push-to-localhost`), which records
every wake-up and when it arrived. The email is a supervised MCP driver this binary also serves
(`serve-mail-driver`), declared `send_external` — `NonIdempotent`, "Can't be unsent" — which
writes each email it sends to a file. Every pass condition is read from the kernel's stores and
routes, the driver's file and the fake Bot API, never from model prose. The program prints a
plain-words transcript and exits non-zero when any pass condition fails.

## Run it

Build the binary once, from the repository root. `scripted-model` lets `up` run on a script
with no key, `tui` is the terminal, `postgres` the shared store and `mobile` the phone:

```bash
cargo build -p adk-cli --bin adk-rust --features scripted-model,tui,postgres,mobile
```

Then, from this directory:

```bash
cargo run                                   # SQLite, scripted model
DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres cargo run -- --store postgres
cargo run -- --live                         # gemini-3.5-flash-lite
cargo run -- --provider anthropic           # claude-sonnet-5-5
```

| Flag | Effect |
|---|---|
| `--store sqlite` | The default: every store in `harness.db` in the kernel home |
| `--store postgres` | Every store in one PostgreSQL database (`up --store`), created for the run on the server `DATABASE_URL` names and dropped after it |
| `--live` | `gemini-3.5-flash-lite` instead of the script; needs `GOOGLE_API_KEY` or `GEMINI_API_KEY`, read from the repository's `.env` (or `SCENARIO_ENV_FILE`) |
| `--provider gemini\|anthropic` | A live run on that provider: `gemini-3.5-flash-lite`, or `claude-sonnet-5-5` with `ANTHROPIC_API_KEY` |
| `--bin PATH` | Another `adk-rust` binary; default `$ADK_RUST_BIN`, else `../../target/debug/adk-rust` |
| `--keep` | Keep the scenario's folder (logs, `harness.db`, the outbox) and database |

Each run has a private `HOME`, kernel home and port, so it never reads or writes your own
configuration, and it stops every program it started on every exit path.

## Time

`adk-rust up` runs on the system clock and has no clock to set from outside, so the night is
compressed: quiet time is set (`[kernel] quiet_hours` with seconds) to a 30-second window 45
seconds after the start (75 in live mode), the routine fires a few seconds after the setup
(`adk-rust wake --cron` with seconds), and the run takes about a minute and a half. Each
transcript line is labelled with the time of the story it stands for and the real time.

## What this proves

| Pass condition (spec 3.3 §12, §11) | How it is checked |
|---|---|
| A stranger gets a pairing code and starts nothing (S-I6) | The fake Bot API sent the unpaired account an `adk-rust pair accept <code>` message; the kernel holds no new agent process and no decision; at the end the account is still not paired and was never sent a card |
| The decision reaches the Telegram thread before quiet time | The card arrived before quiet time started, with **Allow once** and **Deny** only (§7.2) and no address, amount or body (S-I5) |
| The phone is woken for it before quiet time | A wake-up reached the push service on loopback before quiet time started |
| The decision waits through a crash (§7.3, S-I3) | `kill -9` ends the kernel (`SIGKILL`) and its mail driver; after the restart the decision is the same inbox entry at the same version and the routine still waits |
| Nothing notifies during quiet time | No message reached anyone through the fake Bot API, and no wake-up reached the phone's push service, between the start and the end of quiet time, before or after the restart. A decision that waited through a restart is sent to the thread again when quiet time ends |
| Nothing runs before the tap; the email is sent exactly once | The driver's outbox is empty until the tap and holds exactly one email to the client afterwards |
| The web home shows "You said yes in Telegram" | `GET /api/surface/decisions/{id}` answers `decided.text` "You said yes in Telegram at HH:MM", and `/ui/` serves the page |
| The phone's card shows "You said yes in Telegram" | `GET /api/surface/decisions/{id}?surface=phone` with the phone's device token answers the same `decided.text` |
| The terminal shows the same | `adk-rust --output json --session <session>`, attached to the kernel's socket, writes a `decided` line whose card says "You said yes in Telegram at HH:MM" and exits 0 |
| The record names the channel | `GET /api/kernel/record` has the `surface.decide` line of the decision, approved, how `chat`, channel `Telegram` |
| No program is left running | The kernel is reaped and the mail driver's process is gone |

A run on SQLite and a run on PostgreSQL together are the phase's pass (§12: SQLite and one
shared store).

## Sample transcript

```text
  17:54  (23:01:00)  `adk-rust wake --cron "4 1 20 * * *"` creates the invoice routine (63bca19b-…); it sleeps until 23:01:04.
  18:00  (23:01:20)  The routine prepares the email and waits for a decision; Telegram rings.
                    card → Ana: Waiting for you · since 23:01 / Send email / Adk rust · Medium risk / Open in home
                    buttons: Allow once · Deny
  22:00  (23:01:43)  Quiet time starts. Nobody answers.
  23:10  (23:01:45)  `kill -9 87247`: the kernel ends at once, with nothing drained or saved on the way out.
  23:11  (23:01:45)  `adk-rust up` starts again as process 87989.
  07:00  (23:02:13)  Quiet time ends.
  07:30  (23:02:17)  Ana taps Allow once in Telegram.
  07:31  (23:02:31)  The web home shows the decision:
                    GET /api/surface/decisions/… → 200: "You said yes in Telegram at 23:02"
Approve it tomorrow: 15 of 15 pass conditions held
```
