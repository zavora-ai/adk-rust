# surface_morning_after

"The morning after", the phase 2 scenario of spec 3.3 §12 ("While you were away"), run as a
program you can watch.

Over one night two routines finish, one fails, an agent suggests a payment, the person allows
a call from their phone, and the kernel restarts. When quiet time ends the phone reads "While
you were away" once: its counts and spending match what the kernel's stores hold, exactly,
across the restart, and every line links to its record.

The kernel is the `adk-rust up` binary, a real child process. The phone is a paired device:
`adk-rust pair` issues a code, the phone claims it over HTTP and keeps a device token, and
every call it makes carries that token — reading the inbox, answering
(`POST /api/approvals/{id}/decide`), reading "While you were away". The email is a
supervised MCP driver this binary serves (`serve-mail-driver`), declared `send_external`,
which writes each email it sends to a file. The briefing is read from the kernel's own route,
`GET /api/kernel/away` (spec 3.3 §10 K4), with the phone's cursor; the web home's Today (WP
F2) reads the same route. Every pass condition is read from stores and routes, never from
model prose. The program prints a plain-words transcript and exits non-zero when any pass
condition fails.

This program shares its stage (the kernel as a child process, the store, the checks) and its
mail driver with [`surface_approve_tomorrow`](../surface_approve_tomorrow/), through `#[path]`.

## Run it

Build the binary once, from the repository root:

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

The flags are those of `surface_approve_tomorrow`: `--store sqlite|postgres`, `--live`,
`--provider gemini|anthropic`, `--bin PATH`, `--keep`.

## The night

| Story | What happens | How |
|---|---|---|
| 21:00 | The kernel starts; Ana pairs her phone | `adk-rust up`; `adk-rust pair --name "Ana's phone"`, then `ControlClient::claim_pairing` over HTTP |
| 21:02 | Three routines are set | `adk-rust wake --cron` for the bills, the weekly report and the bank statement |
| 22:00 | Quiet time starts | `[kernel] quiet_hours` |
| 23:00 | The bills routine suggests paying the electricity bill | The model calls `propose_followup`; the suggestion waits in the inbox, and nothing runs |
| 01:00 | The report routine asks to send an email; Ana allows it on her phone | `send_email` waits; the phone answers with its device token; the email is sent once |
| 02:00 | The kernel is killed and starts again; its model can no longer be reached | `kill -9`; scripted mode starts it with an empty script, live mode with a key the provider refuses |
| 04:00 | The statement routine fails | Its model call fails |
| 06:59 | The night is on the phone's page; quiet time still holds the briefing back | `GET /api/kernel/away` with the device token, and the quiet time `GET /api/kernel/settings/default` serves |
| 07:00 | Quiet time ends; the phone reads "While you were away" | `GET /api/kernel/away` with the device token |
| 07:05 | Ana marks it seen; the kernel restarts again; she reads it again | `POST /api/kernel/away/seen`; nothing comes back |

The night is compressed: `adk-rust up` runs on the system clock, so quiet time is a one-minute
window from the first whole minute at least 40 seconds after the start, which the `HH:MM` quiet
time the kernel serves names exactly, and the routines fire inside it. Each transcript line is
labelled with the time of the story it stands for and the real time.

## What this proves

| Pass condition (spec 3.3 §12, §10 K4) | How it is checked |
|---|---|
| The phone's answer runs the call once | The inbox answers `approved`; the driver's outbox holds exactly one email; the routine's run completes |
| Its counts match the ledger exactly across the restart | The briefing's 2 done, 1 failed, 1 suggestion and 1 decided equal the routines' runs in the process table (two completed, one failed), the agenda (one item) and the decision; its `waiting` equals the inbox |
| One briefing at 07:00, not before | Four seconds before quiet time ends the phone's page already holds the night, and the quiet time the kernel serves still holds, so the home's rule for the morning briefing (`briefingDue` in `adk-surface/web/src/away.ts`) holds it back; once quiet time ends it is due |
| Its spending matches across the restart | `spentMicroUsd`, the `/proc` ledger's spend rows, equals the kernel's daily total in its process store (`GET /api/kernel/settings/default`, `spentToday`), and is not zero. Each scripted reply names its price (`"cost"` in `--model-script`), so the scripted run spends exactly the sum of the evening's four replies, 41,000 micro-USD; the live run compares real spend |
| Quiet time was respected | The briefing reports no suggestion's work started in quiet time: the payment waited for Ana |
| Every line links to its record | Each line's `record`, `GET /api/kernel/away/lines/{seq}` with the phone's token, answers its ledger row: same `seq`, same kind |
| No line appears twice across the restart | Line sequence numbers are strictly increasing |
| Exactly one briefing | Once the phone marks it seen it does not come back, after another restart too; the web home's own cursor still has the night to read |
| The phone's answer is on record | `GET /api/kernel/record` has a line for the decision |
| No program is left running | The kernel is reaped and the mail driver's process is gone |

A run on SQLite and a run on PostgreSQL together are the phase's pass (§12: SQLite and one
shared store).

## Sample transcript

```text
  23:00  (23:03:38)  The bills routine suggests "Pay electricity bill ($84.20)"; it waits for Ana, and nothing is paid.
  01:00  (23:03:38)  The report routine asks to send an email. Ana taps Allow once on her phone (`POST /api/approvals/{id}/decide` with the device token).
  02:00  (23:03:41)  `kill -9 88242` (signal 9), and `adk-rust up` starts again as process 88738; from now on its model cannot be reached.
  04:00  (23:03:53)  The statement routine fails: its model call gets no answer.
  07:00  (23:04:12)  Quiet time ends. The phone reads "While you were away" (`GET /api/kernel/away`):
                    23:03:36 Suggestion kernel → /api/kernel/away/lines/43
                    23:03:38 Done adk-rust → /api/kernel/away/lines/53
                    23:03:38 Decided urn:adk:agent:adk-rust/adk-rust → /api/kernel/away/lines/70
                    23:03:40 Done adk-rust → /api/kernel/away/lines/73
                    23:03:53 Failed adk-rust → /api/kernel/away/lines/93
                    counts AwayCounts { done: 2, failed: 1, stopped: 0, suggestions: 1, questions: 0, decided: 1 }; spent 6308 micro-USD; quiet time respected: true; waiting 1
The morning after: 10 of 10 pass conditions held
```
