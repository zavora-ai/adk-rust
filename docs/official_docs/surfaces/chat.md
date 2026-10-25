# Agent Surfaces: Chat

A chat thread is the doorbell and the conversation (spec 3.3 §5.3): the person talks to
their agents, gets a card when a decision waits, says yes once or no, and stops everything.
The `channels` feature of `adk-surface` ships the Telegram adapter. It moves messages
between a Telegram bot and the kernel's [channel gateway](../kernel/pairing.md#chat-channels),
which applies every rule: pairing, what a card carries, what chat may decide, stopping.

> **Note:** chat is Beta. `adk-rust up` starts the adapter when `TELEGRAM_BOT_TOKEN` is set
> ([With `adk-rust up`](#with-adk-rust-up)); a program that hosts its own kernel starts it as
> in [Running the adapter](#running-the-adapter).

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["channels"] }
```

## What reaches Telegram

A chat card leaves the machine through Telegram's servers, so it carries a short summary,
the agent and the risk, and nothing of the call's arguments (S-I5):

| Part | Example | From |
|---|---|---|
| Status and wait | Waiting for you · since 18:00 | When it was asked, in the host's time zone |
| Heading | **Write file** | The call in plain words, as the engine heads every chat card: the tool, never its arguments. Without the approval service at hand, the kernel's summary: at most 160 characters, every argument value masked as `[…]` |
| Summary | Send the October invoice to […] | The kernel's masked summary, when an approval route wrote one; never the runtime's own "Tool 'write_file' requires confirmation: …" |
| Who and risk | Finance · Medium risk | The asking agent, and the risk in the words of spec 3.3 §8 |
| Buttons | **Allow once** · **Deny** | The answers the card offers, and no others; none at high risk or for a call that moves money |
| Link | Open in home | The gateway's `home_url`, for everything chat may not decide |

A card chat may not answer says why instead of offering buttons: "This always needs you, on a
screen. Voice, chat and standing permissions can't say yes to it." The decision's id and
version ride in the buttons' data, which Telegram returns on a press; they are never shown.

| Sent | As |
|---|---|
| An agent's reply | Plain text, never read as markup; split into messages of Telegram's length |
| A card or a notice | Telegram HTML, every string from a model, a tool or a person escaped (S-I9) |
| Any text | Without control characters or the bidirectional controls that reorder what a reader sees, and with link previews off |

Text the person types reaches the agent as their words, so it loses every character Telegram
shows as nothing: control and bidirectional characters, zero-width characters and joiners,
variation selectors, and the tag block, which a model reads back as ASCII (S-I6). An emoji
sequence keeps its emoji and loses its joiners.

## What the thread may do

| The person | What happens |
|---|---|
| Writes from an account that is not paired | The account is sent a pairing code; nothing starts (S-I6). Accept it where the kernel runs: `adk-rust pair accept CODE` |
| Writes from a paired account | A turn starts as the person, in the thread's session; the reply comes back to the thread |
| Presses **Allow once** or **Deny** | The decision is answered at the version the card showed. The press is acknowledged, the card loses its buttons, and the answer is confirmed under it: "You allowed it once · 07:31" |
| Presses a button on a card someone answered first | "Decided already: You said yes at 07:31." Nothing is decided twice (S-I2) |
| Types "allow once", "yes" or anything else | A message to the agent. Only a pressed button answers a decision (S-I4) |
| Types "stop all" or `/stop` | Every agent stops: "All agents stopped. Use Resume all in the home app." (S-I10) |
| Types "resume" or `/resume` | Refused: "Resuming happens only in the home." |
| Sends a message or presses a button the kernel cannot take | Told so in a reply: "That message didn't reach your agents. Try sending it again." or "That answer wasn't recorded, so nothing was decided. Try again, or decide in the home." |

The adapter reads new text messages and button presses from people in private chats.
Messages in groups and channels, messages from bots, edits, photos and other media are
ignored, so a card never reaches a thread other people read. `/start` reaches the gateway
without a deep link's payload, so a link anyone can share cannot put words in the person's
mouth. `/stop` is listed among the bot's commands as **Stop all**.

## With `adk-rust up`

1. Create a bot with [@BotFather](https://t.me/BotFather).
2. Start the kernel with its token:

   ```bash
   TELEGRAM_BOT_TOKEN=123456789:AA… adk-rust up
   ```

   The banner says `chat  Telegram` and where cards link to. Without the variable nothing of
   the chat thread is built, started or logged.
3. Write to the bot from your Telegram account. It answers with a pairing code; accept it
   where the kernel runs:

   ```bash
   adk-rust pair accept 3HWT-9PQA-7KMX
   ```

4. The account is listed by `adk-rust pair list` and in the home under **Where you can say
   yes from**, where it can be removed. Its messages are turns on the harness, and every
   decision rings its thread.

| Variable | Effect |
|---|---|
| `TELEGRAM_BOT_TOKEN` | Starts the chat thread with this bot |
| `TELEGRAM_API_BASE` | Another Bot API address, such as a local Bot API server |
| `TELEGRAM_HOME_URL` | The home's address as your phone opens it, which cards and notices link to, such as `http://192.168.1.20:8080/ui/`. Without it, cards link to the kernel's bind address, with `127.0.0.1` in place of `0.0.0.0` |

| Rule | Why |
|---|---|
| `--mode auto` with `TELEGRAM_BOT_TOKEN` needs `--sandbox` | A paired account's messages come from outside the machine, so edits would run unattended on this host, as `up` refuses beyond loopback |
| One replica sets `TELEGRAM_BOT_TOKEN` | Telegram lets one process read a bot's updates; a second one is logged as "another process reads this bot's updates" |
| The home is served while waiting cards are sent | Cards of decisions that waited through a restart are sent in the background, so an unreachable Telegram never holds up the home |
| A refused token stops the reading, not `up` | The error says to restart with a working token; the home and the terminal keep working |

Paired accounts are kept where the kernel's paired devices are: `harness.db`, the `--store`
backend (PostgreSQL or Redis), or memory with `--ephemeral`. Each answer given in Telegram is
written to the kernel's signed audit log, so the home says "You said yes in Telegram at
07:31" after a restart too, and so does `adk-rust` attached to the kernel, which reads the
kernel's record. With `--ephemeral`, which keeps no audit log, the thread and the home share
how each answer was given in memory while `up` runs; the attached terminal says "You said yes
at 07:31". The feature is `adk-cli`'s `channels`, on by default.

Every call `up` holds is medium risk, so its card offers **Allow once** and **Deny**. The card
names the tool ("Bash", "Write file") and never its arguments, so the command or the file is
seen only in the home, through "Open in home".

## Running the adapter

1. Create a bot with [@BotFather](https://t.me/BotFather) and keep its token in
   `TELEGRAM_BOT_TOKEN`. Each bot serves one kernel.
2. Build the gateway over the kernel with the adapter as its sink, and start the polling:

```rust,ignore
use std::sync::Arc;

use adk_core::ApprovalService;
use adk_core::audit_log::AuditTrail;
use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
use adk_kernel::Kernel;
use adk_surface::channels::TelegramAdapter;
use adk_surface::{AnswerLog, SurfaceEngine};

async fn chat(
    kernel: Kernel,
    approvals: Arc<dyn ApprovalService>, // the kernel's approval service
    doorbell: Arc<ChannelDoorbell>,        // the notifier of the kernel's approval service
    bindings: Arc<InMemoryChannelBindingStore>, // the store the control plane's pairing uses
    answers: Arc<dyn AnswerLog>,           // the log the engine was built with
    trail: AuditTrail,                     // the engine's audit trail
) -> Result<(), Box<dyn std::error::Error>> {
    let telegram = TelegramAdapter::builder(std::env::var("TELEGRAM_BOT_TOKEN")?)
        .approvals(approvals)
        .answer_log(answers)
        .audit(trail)
        .build()?;
    let gateway = ChannelGateway::builder(Arc::new(kernel), bindings)
        .sink(Arc::new(telegram.clone()))
        .home_url("http://127.0.0.1:8080/ui/")
        .doorbell(doorbell)
        .build();
    gateway.start().await?;
    // A decision that waited through a restart is rung again.
    gateway.ring_waiting().await?;
    // The polling holds the gateway; this clone keeps its replies and cards going after it.
    let polling = telegram.start(gateway.clone());
    polling.join().await?;
    Ok(())
}
```

[Building the gateway](../kernel/pairing.md#building-the-gateway) shows the kernel, the
approval service with its doorbell, and the binding store the control plane pairs accounts
in.

| Builder method | Effect |
|---|---|
| `api_base(url)` | The Bot API's address; `https://api.telegram.org` by default. Plain `http` is accepted on loopback only, since the token travels in the request path |
| `poll_timeout(d)` | How long one long poll waits for an update; 25 s by default |
| `utc_offset(offset)` | The time zone of every time the thread shows: "since 18:00", "07:31", "Decided already: You said yes at 07:31." and a pairing code's "before 08:31". The host's by default, daylight saving time included |
| `approvals(service)` | Reads each card's decision, so the card is headed by the call in plain words |
| `answer_log(log)` | Keeps each answer given in Telegram in the engine's `AnswerLog` |
| `audit(trail)` | Writes a `surface.decide` record of each answer given in Telegram |

### "You said yes in Telegram"

The approval service records who decided, not where. With `answer_log` and `audit` given the
engine's log and trail, the surfaces on that engine — the web home and the phone — show a
decision answered in Telegram as "You said yes in Telegram at 07:31". A surface on another
engine shows it when its answer log reads the same records: an `AuditAnswerLog` over the same
signed audit log, or a `RecordAnswerLog` over the kernel's record, as the terminal attached to
a kernel does. The `surface.decide` record carries `approvalId`, `answer`,
`outcome: "decided"`, `surface: "chat"` and `origin: {"kind": "chat", "channel": "Telegram"}`,
so an `AuditAnswerLog` over the signed audit log finds it after a restart. It is written when
the answer decided, before the confirmation is sent to the thread, so a slow Telegram or a
stop cannot lose it; a quorum's earlier answers, which decide nothing yet, are not recorded as
deciding. The gateway's own `kernel.channel` record of the same answer names the account and
the channel `telegram`.

### Long polling

The adapter reads updates with `getUpdates`, so the kernel needs no inbound port and the home
stays on loopback (spec 3.3 §13 decision 10).

| Event | What the adapter does |
|---|---|
| Updates arrive | Hands each to the gateway, a paired account's first and each account's in the order sent, then acknowledges them on the next poll; a restart reads an unacknowledged update again, and the gateway starts one turn per message |
| The gateway fails on a paired account's update, and the failure may clear (a store outage, a timeout) | Reads it again after 1, 2, 4 and 8 seconds; that account's later updates wait with it, while the other accounts' updates already read are handed over. After the fifth failure it is dropped, and the person is told |
| The gateway fails on any other update | Drops it at once; a paired person is told, a stranger is not. A stranger who blocks the bot holds up no one |
| Telegram is unreachable, or another process reads the bot's updates (`409`) | Retries with backoff up to a minute; a `409` is logged once as an error |
| Telegram rate-limits a call (`429`) | Waits the time Telegram asks, up to 30 seconds, twice; a reply to an account that is not paired is never waited for |
| Telegram refuses the token | Ends the polling: `TelegramPolling::join` returns `TelegramError::Unauthorized`. The gateway's replies and cards go on while the caller keeps a handle to it |

The bot token never appears in an error, a log line or a `Debug` output.

> **Note:** webhooks are not used. Telegram signs nothing: it sends a shared secret header,
> and the kernel's `WebhookIngress` checks Standard Webhooks signatures and starts runs
> directly, which would skip the gateway's pairing.

## Testing an adapter

The adapter passes the kernel's `channel_contract_tests!` against a fake Bot API on
loopback: what the suite reads as the wire is every call the adapter made to Telegram, and
what the person types and presses goes through the adapter's own reading of Telegram
updates. `adk-surface/tests/telegram_tests.rs` runs the scenario end to end on a
kernel-hosted harness with a scripted model.

```bash
cargo nextest run -p adk-surface --features channels
```

## Related

- [Pairing Devices and Chat Channels](../kernel/pairing.md) — the gateway, its rules, the binding store and the contract suite
- [The Engine](index.md) — decision cards, what each surface may decide, the answer log
- [The Web Home](web-home.md) — where a card that chat may not answer is decided
