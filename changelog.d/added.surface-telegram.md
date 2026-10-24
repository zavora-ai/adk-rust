- **The Telegram chat adapter** (`adk-surface` feature `channels`; spec 3.3 §5.3, Beta):
  `adk_surface::channels::TelegramAdapter` is a Telegram bot as the person's chat thread with
  their agents, over the kernel's channel contract (`adk-kernel` feature `channels`). It is the
  gateway's `ChannelSink` for the channel `telegram` and reads the bot's updates by long
  polling (`TelegramAdapter::start`), so the kernel needs no inbound port. A decision card
  reads "Waiting for you · since 18:00", the kernel's summary with every argument value
  masked, the agent and the risk, with **Allow once** and **Deny** buttons only where the card
  offers them and an "Open in home" link; the call's arguments never reach Telegram, and
  every string is escaped. Only a pressed button answers a decision; the press is
  acknowledged, the card loses its buttons, and the answer is confirmed under it ("You allowed
  it once · 07:31"). Messages in groups and channels, from bots, edits and media are ignored,
  and `/start` loses a deep link's payload. `/stop` is listed as **Stop all**. With
  `answer_log` and `audit` given the engine's answer log and trail, every other surface shows
  "You said yes in Telegram at 07:31", after a restart too. The adapter passes
  `channel_contract_tests!`; the bot token never appears in an error, a log line or `Debug`.
