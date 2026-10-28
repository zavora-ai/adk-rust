- **WhatsApp, Slack, Discord and Matrix chat adapters** (`adk-surface`, Experimental; spec 3.5
  §10 X12): four adapters beside Telegram's on the kernel's channel contract, one feature each
  (`whatsapp`, `slack`, `discord`, `matrix`, each implying `channels`), each passing
  `channel_contract_tests!` against a fake of its platform. `WhatsAppAdapter` uses the
  official Cloud API only: its webhook router refuses a body whose `X-Hub-Signature-256` is
  not the HMAC-SHA256 of the raw body with the app secret, compared in constant time, before
  reading it; answers the verification handshake; takes a replayed message once; sends cards
  as at most three reply buttons of twenty characters; and outside the 24-hour customer-service
  window sends nothing but an approved template, once, while the send fails with
  `WhatsAppError::OutsideWindow`. `SlackAdapter` reads Socket Mode, acknowledging every
  envelope, and sends Block Kit. `DiscordAdapter` keeps a Gateway session with heartbeat,
  resume and reconnect, answers each interaction through its callback, declares `/stop`, and
  sends `allowed_mentions: {"parse": []}`. `MatrixAdapter` reads `/sync`, lists a card's
  answers as reaction keys and takes only a reaction from the account paired from the card's
  room, in unencrypted direct chats. Every adapter escapes its platform's markup, drops the
  characters a chat app shows as nothing from typed text, hands a paired account's messages
  over first, and never waits out a rate limit for a stranger.
- **`adk-rust up` starts Slack, Discord and Matrix** (`adk-cli`): each when its variables are
  set (`SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN`; `DISCORD_BOT_TOKEN`; `MATRIX_HOMESERVER` and
  `MATRIX_ACCESS_TOKEN`), with accounts paired by `adk-rust pair accept` beside Telegram's.
  The features are `adk-cli`'s `slack`, `discord` and `matrix`, on by default. `up` does not
  mount WhatsApp's webhook: its home stays on loopback.
