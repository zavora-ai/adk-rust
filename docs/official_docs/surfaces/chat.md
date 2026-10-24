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
| Status and wait | Waiting for you · since 18:00 | The decision's age, at the host's time zone |
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

   The banner says `chat  Telegram`. Without the variable nothing of the chat thread is built,
   started or logged. `TELEGRAM_API_BASE` points the bot at another Bot API address, such as
   a local Bot API server.
3. Write to the bot from your Telegram account. It answers with a pairing code; accept it
   where the kernel runs:

   ```bash
   adk-rust pair accept 3HWT-9PQA-7KMX
   ```

4. The account is listed by `adk-rust pair list` and in the home under **Where you can say
   yes from**, where it can be removed. Its messages are turns on the harness, and every
   decision rings its thread.

Paired accounts are kept where the kernel's paired devices are: `harness.db`, the `--store`
backend (PostgreSQL or Redis), or memory with `--ephemeral`. Cards link to the home at the
kernel's bind address, and each answer given in Telegram is written to the kernel's signed
audit log, so the home says "You said yes in Telegram at 07:31" after a restart too. The
feature is `adk-cli`'s `channels`, on by default.

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
    let polling = telegram.start(gateway);
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
| `utc_offset(offset)` | The time zone of "since 18:00" and "07:31"; the host's by default |
| `approvals(service)` | Reads each card's decision, so the card is headed by the call in plain words |
| `answer_log(log)` | Keeps each answer given in Telegram in the engine's `AnswerLog` |
| `audit(trail)` | Writes a `surface.decide` record of each answer given in Telegram |

### "You said yes in Telegram"

The approval service records who decided, not where. With `answer_log` and `audit` given the
engine's log and trail, every other surface shows a decision answered in Telegram as
"You said yes in Telegram at 07:31": the web home, the phone and the terminal. The
`surface.decide` record carries `approvalId`, `answer`, `outcome: "decided"`, `surface: "chat"`
and `origin: {"kind": "chat", "channel": "Telegram"}`, so an `AuditAnswerLog` over the signed
audit log finds it after a restart. The gateway's own `kernel.channel` record of the same
answer names the account and the channel `telegram`.

### Long polling

The adapter reads updates with `getUpdates`, so the kernel needs no inbound port and the home
stays on loopback (spec 3.3 §13 decision 10).

| Event | What the adapter does |
|---|---|
| An update arrives | Hands it to the gateway, then acknowledges it on the next poll; a restart reads an unacknowledged update again, and the gateway starts one turn per message |
| The gateway fails on an update | Reads it again, waiting 1, 2, 4 and 8 seconds, and skips it after the fifth failure |
| Telegram is unreachable, or another process reads the bot's updates (`409`) | Retries with backoff up to a minute |
| Telegram rate-limits a call (`429`) | Waits the time Telegram asks, up to 30 seconds, twice |
| Telegram refuses the token | Ends the polling: `TelegramPolling::join` returns `TelegramError::Unauthorized` |

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
