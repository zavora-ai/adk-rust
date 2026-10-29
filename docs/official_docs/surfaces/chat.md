# Agent Surfaces: Chat

A chat thread is the doorbell and the conversation (spec 3.3 §5.3): the person talks to
their agents, gets a card when a decision waits, says yes once or no, and stops everything.
The `channels` feature of `adk-surface` ships the Telegram adapter. It moves messages
between a Telegram bot and the kernel's [channel gateway](../kernel/pairing.md#chat-channels),
which applies every rule: pairing, what a card carries, what chat may decide, stopping. The
`whatsapp`, `slack`, `discord` and `matrix` features add adapters for those platforms
([WhatsApp, Slack, Discord and Matrix](#whatsapp-slack-discord-and-matrix)).

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

## WhatsApp, Slack, Discord and Matrix

Four more adapters sit beside Telegram's (spec 3.5 §10 X12), one feature each, so a
Telegram-only build stays lean. Each is a `ChannelSink` for the gateway and keeps the rules
the adapter keeps on Telegram: only a pressed answer decides (S-I4); a card offers its own
answers and no others, none at high risk or for money; "stop all" and the platform's `/stop`
reach the gateway; every string from a model, a tool or a person is escaped for the
platform's markup (S-I9); the characters a chat app shows as nothing are dropped from typed
text (S-I6); a paired account's messages are handed over first, and nothing sent to a
stranger waits out a rate limit. The cards read as Telegram's do, in the same words.

| Feature | Adapter | Channel | In | Out | Answers |
|---|---|---|---|---|---|
| `whatsapp` | `WhatsAppAdapter` | `whatsapp` | A webhook the host mounts on a public HTTPS address | Graph API `/{phone-number-id}/messages` | Interactive reply buttons: at most three, titles of twenty characters at most |
| `slack` | `SlackAdapter` | `slack` | Socket Mode: a WebSocket the adapter opens, every envelope acknowledged | Web API `chat.postMessage`, `chat.update`, Block Kit | Buttons (`block_actions`) |
| `discord` | `DiscordAdapter` | `discord` | The Gateway: a WebSocket the adapter opens, with heartbeat, resume and reconnect | REST create message with button components | Buttons (`INTERACTION_CREATE`), each answered through its interaction callback |
| `matrix` | `MatrixAdapter` | `matrix` | `/sync` long polling with the `since` token | `m.room.message` with `body` and an HTML `formatted_body` | Reactions: the card lists its answers as reaction keys |

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["slack", "discord", "matrix", "whatsapp"] }
```

Each feature implies `channels`. Every adapter is built like Telegram's — `approvals`,
`answer_log`, `audit`, `card_hints` and `utc_offset` on its builder — and started with
`start(gateway)`, which returns a `ChannelConnection` (`SlackConnection`, `DiscordConnection`,
`MatrixSync`) to stop or join. A refused token ends it with the platform's `Unauthorized`
error. No token appears in an error, a log line or a `Debug` output.

### Slack

1. Create a Slack app with Socket Mode on. Give it an app-level token with
   `connections:write` (`SLACK_APP_TOKEN`, `xapp-…`), and a bot token with `chat:write` and
   `im:history` (`SLACK_BOT_TOKEN`, `xoxb-…`).
2. Under **Event Subscriptions**, subscribe the bot to `message.im`; turn **Interactivity**
   on; under **App Home**, let people send messages to the app. To offer `/stop`, declare it
   as a slash command; Socket Mode delivers it without a request URL.

| What | How it reaches Slack |
|---|---|
| A card | Block Kit: the heading as a header block, every other line as `plain_text`, which Slack never reads as markup; the answers as buttons whose values carry the decision and version; "Open in home" as a link button. The answer buttons are removed once the card is answered (`chat.update`) |
| An agent's reply | `text` with `&`, `<` and `>` escaped and `mrkdwn` off, in messages of at most 3000 characters |
| What is read | Messages in a direct message with the app (`channel_type: im`), button presses and `/stop` from a direct message. Channels, bots, edits and files are ignored |
| A `disconnect` envelope, a closed or quiet socket | A new socket is opened with `apps.connections.open`; Slack delivers again what was not acknowledged |
| `429` | `Retry-After` is waited out, up to 30 seconds, twice; never for a stranger |

### Discord

1. Create an application in the Discord Developer Portal, add a bot and copy its token
   (`DISCORD_BOT_TOKEN`). The adapter asks only for the `DIRECT_MESSAGES` intent, whose
   content needs no privileged intent.
2. Install the app for yourself, or share a server with the bot, so it can receive your
   direct messages. The adapter declares `/stop` as the bot's application command when it
   connects.

| What | How it reaches Discord |
|---|---|
| A card | Content with Discord's markdown escaped, the heading in bold, the answers as buttons whose custom ids carry the decision and version, "Open in home" as a link button, `allowed_mentions: {"parse": []}` and embeds suppressed. An answered card loses its answer buttons |
| A press | Answered through the interaction callback at once (`DEFERRED_UPDATE_MESSAGE`); the confirmation replies to the card |
| `/stop` | Answered with a deferred reply, which the stop notice becomes |
| An agent's reply | Every character that formats, links, quotes, lists or mentions escaped with a backslash, and no mention pings anyone |
| What is read | `MESSAGE_CREATE` in a direct message and `INTERACTION_CREATE`. Servers, bots, edits and system messages are ignored |
| Reconnect (op 7), a missed heartbeat acknowledgement, a failed socket | Connects again to `resume_gateway_url` and resumes the session with its last sequence number |
| Invalid session (op 9) | Resumes when Discord says it can; otherwise identifies anew |
| Close 4004, or 4010–4014 | Ends the connection: `DiscordError::Unauthorized`, or `DiscordError::Gateway` for disallowed intents |
| `429` | `retry_after` is waited out, up to 30 seconds, twice; never for a stranger |

### Matrix

1. Create an account for the agents on a homeserver and log it in for an access token
   (`MATRIX_HOMESERVER`, `MATRIX_ACCESS_TOKEN`).
2. Start a direct chat with that account **with encryption off**. The adapter joins a room
   it is invited to as a direct chat (`is_direct`), and no other.

Matrix has no buttons. A card lists its answers as reaction keys — "To answer, react 👍
Allow once · 👎 Deny" — and the adapter puts its own reaction with each key on the card, so
the person has only to tap. Only an `m.reaction` annotation on that card, from the account
paired from that room, with one of the keys the card offers, answers it. A reaction from
another account, with another key, or on a message the adapter did not send decides nothing,
and nothing is said in the room. The card names its decision in a field of its own content,
so a card the adapter no longer remembers is read back from the homeserver after a restart.

| What | How it reaches Matrix |
|---|---|
| A card or a notice | `m.room.message` with a plain `body` and a `formatted_body` whose every string is HTML-escaped; a notice about a card replies to it |
| An agent's reply | A plain `body` and no `formatted_body`, so nothing in it is read as markup |
| What is read | Text messages and reactions in joined direct chats. Edits, notices, the account's own events and rooms of more than two members are ignored; so is everything sent before the adapter started |
| An encrypted room | Told once to use an unencrypted direct chat; nothing in it reaches an agent |
| `429` (`M_LIMIT_EXCEEDED`) | `retry_after_ms` is waited out, up to 30 seconds, twice; never for a stranger |

### WhatsApp

The adapter uses the official WhatsApp Business Cloud API only. WhatsApp reaches it through a
webhook, so it needs a public HTTPS address.

1. In a Meta app with the WhatsApp product, note the business phone number's id
   (`WHATSAPP_PHONE_NUMBER_ID`), a system user's access token with
   `whatsapp_business_messaging` (`WHATSAPP_ACCESS_TOKEN`), and the app secret
   (`WHATSAPP_APP_SECRET`). Choose a verify token (`WHATSAPP_VERIFY_TOKEN`).
2. Mount the webhook's router at a path on the public address, such as
   `/channels/whatsapp`, and give WhatsApp that URL and the verify token. Subscribe it to the
   `messages` field.
3. Optionally, have a template approved with no variables, such as "You have a decision
   waiting. Reply to see it.", and name it with `template(name, language)`.

```rust,ignore
use std::sync::Arc;

use adk_kernel::Kernel;
use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
use adk_surface::channels::{WhatsAppAdapter, WhatsAppWebhook};

async fn whatsapp(
    kernel: Kernel,
    doorbell: Arc<ChannelDoorbell>,             // the notifier of the kernel's approval service
    bindings: Arc<InMemoryChannelBindingStore>, // the store the control plane's pairing uses
) -> Result<(axum::Router, WhatsAppWebhook), Box<dyn std::error::Error>> {
    let whatsapp = WhatsAppAdapter::builder(
        std::env::var("WHATSAPP_PHONE_NUMBER_ID")?,
        std::env::var("WHATSAPP_ACCESS_TOKEN")?,
    )
    .app_secret(std::env::var("WHATSAPP_APP_SECRET")?)
    .verify_token(std::env::var("WHATSAPP_VERIFY_TOKEN")?)
    .template("decision_waiting", "en_US")
    .build()?;
    let gateway = ChannelGateway::builder(Arc::new(kernel), bindings)
        .sink(Arc::new(whatsapp.clone()))
        .home_url("https://home.example/ui/")
        .doorbell(doorbell)
        .build();
    gateway.start().await?;
    let webhook = whatsapp.start(gateway);
    // Served on the public address; dropping `webhook` stops the handing over.
    let router = axum::Router::new().nest("/channels/whatsapp", webhook.router());
    Ok((router, webhook))
}
```

| What | What the adapter does |
|---|---|
| `GET` with `hub.mode=subscribe` | Answers `hub.challenge` when `hub.verify_token` matches, compared in constant time; otherwise `403` |
| `POST` | Refused with `401` before it is read unless `X-Hub-Signature-256` is the HMAC-SHA256 of the raw body with the app secret, compared in constant time; then answered `200` and handed over |
| A message delivered again | Acknowledged and not taken again: its `wamid` was seen |
| A card | An interactive message: the chat screen's lines, a reply button for each answer the card offers, "Open in home" as its address |
| An agent's reply | Text with `*`, `_`, `~` and `` ` `` between zero-width spaces, and a quote or list marker at a line's start after one, so nothing formats; WhatsApp has no escape character |
| Outside the 24-hour customer-service window | Nothing is sent: the send fails with `WhatsAppError::OutsideWindow`, so the gateway knows, and the approved template, when set, is sent once in its place until the person writes again. After a restart the adapter has not seen anyone write, so WhatsApp decides (error `131047`), with the same result |
| A throughput limit | Tried again after 1 and 2 seconds; never for a stranger |

> **Note:** `adk-rust up` does not mount the WhatsApp webhook. Its home stays on loopback
> (spec 3.3 §13 decision 10), and a webhook needs a public address. A host with a tunnel or a
> reverse proxy mounts `WhatsAppWebhook::router()` itself, on the kernel the gateway runs on.

### Photos

Every adapter downloads the images a paired account's message carried into
`ChannelInbound::attachments` — JPEG, PNG, WebP and GIF, at most 10 MiB each and ten a
message, the size checked while streaming — and never a stranger's. A photo's caption is the
message's words.

| Platform | Images | Downloaded from |
|---|---|---|
| WhatsApp | `image` messages | The Graph API's media address, with the access token |
| Telegram | `photo`, its largest size within the limit | `getFile`, then the Bot API's file address |
| Slack | `files` of a `file_share` message | `url_private_download` on `files.slack.com`, with the bot token; the app needs `files:read` |
| Discord | `attachments` of a message, by `content_type` | Discord's CDN (`cdn.discordapp.com`, `media.discordapp.net`), without the bot token |
| Matrix | `m.image` events; the body is the caption when a `filename` is given | The `mxc://` URI through `/_matrix/client/v1/media/download`, with the access token, else the legacy `/_matrix/media/v3/download` |

A file whose declared type or size the limits rule out is not downloaded, and a token goes to
its platform's own file host only. The gateway never puts attachments in a turn's input, and a
paired account's message with no words, a photo alone, starts no turn
(`InboundOutcome::Empty`): a host that takes photos does so with an `InboundRoute`, as a served
site does.

### On a served site

A website served by `adk-website` runs one gateway over the platforms of its project's
`channels.json`, built by `adk-rust website new`, with a route of its own: a paired account's
message is a turn of the Steward, a stranger's goes to the site's Inbox and never gets a
pairing code or starts a turn, and only `/pair` — Slack's and Discord's `/pair` command too —
gets a stranger a code. The site's public edge serves WhatsApp's webhook at
`/channels/whatsapp`. See
[Chat on a served site](../deployment/adk-website.md#chat-on-a-served-site).

### With `adk-rust up`

`adk-rust up` starts Slack, Discord and Matrix as it starts Telegram, each only when its
variables are set; without them nothing of it is built, started or logged. None needs an
inbound port, so the home keeps its loopback bind.

| Variables | Starts |
|---|---|
| `SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN` (`SLACK_API_BASE` for another address) | Slack |
| `DISCORD_BOT_TOKEN` (`DISCORD_API_BASE` for another address) | Discord |
| `MATRIX_HOMESERVER` and `MATRIX_ACCESS_TOKEN` | Matrix |

```bash
SLACK_APP_TOKEN=xapp-… SLACK_BOT_TOKEN=xoxb-… DISCORD_BOT_TOKEN=… adk-rust up
```

The banner says `chat  Slack, Discord` and where cards link to: the phone's home when `up`
serves one, otherwise the bind address. Accounts write to the bot, get a pairing code, and are
paired with `adk-rust pair accept <code>` in the store Telegram's accounts are in; they are
listed by `adk-rust pair list` and in the home under **Where you can say yes from**. One of a
pair of variables without the other stops `up` with an error, and `--mode auto` needs
`--sandbox`, as with Telegram. The features are `adk-cli`'s `slack`, `discord` and `matrix`,
on by default.

## Testing an adapter

The adapter passes the kernel's `channel_contract_tests!` against a fake Bot API on
loopback: what the suite reads as the wire is every call the adapter made to Telegram, and
what the person types and presses goes through the adapter's own reading of Telegram
updates. `adk-surface/tests/telegram_tests.rs` runs the scenario end to end on a
kernel-hosted harness with a scripted model.

The other four pass the same suite against fakes of their platforms in
`adk-surface/tests/fake_<platform>/` — HTTP for all four, and a WebSocket for Slack's Socket
Mode and Discord's Gateway — in `<platform>_contract.rs`, and `<platform>_tests.rs` covers
what each keeps: strangers, answers given twice, typed text, escaping, `/stop`, rate limits,
reconnects, and each platform's own rules (WhatsApp's signatures, handshake, window and button
limits; Slack's acknowledgements; Matrix's reactions and encrypted rooms). A live test per
platform is `#[ignore]`d and reads its variables from the environment.

```bash
cargo nextest run -p adk-surface --features channels
cargo nextest run -p adk-surface --features whatsapp,slack,discord,matrix
```

## Related

- [Pairing Devices and Chat Channels](../kernel/pairing.md) — the gateway, its rules, the binding store and the contract suite
- [The Engine](index.md) — decision cards, what each surface may decide, the answer log
- [The Web Home](web-home.md) — where a card that chat may not answer is decided
