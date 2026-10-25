- **A stranger can no longer hold up the Telegram thread, and hidden text no longer reaches an
  agent** (`adk-surface`, `adk-cli`; spec 3.3 S-I6, S-I10):
  - `TelegramAdapter` hands a paired account's updates to the gateway before a stranger's. An
    update the gateway fails on is read again only when it is a paired account's and the
    failure may clear (a store outage, a timeout), holding only that account's later updates;
    any other is dropped at once. A stranger who writes and blocks the bot no longer delays a
    paired person's "stop all" or answer by about 15 s per message, and a reply to an account
    that is not paired never waits out a rate limit.
  - Typed text loses the characters Telegram shows as nothing before it becomes the person's
    words: zero-width characters, the word joiner, the byte order mark, variation selectors and
    the Unicode tag block, which a model reads back as ASCII.
  - `adk-rust up` with `TELEGRAM_BOT_TOKEN` refuses `--mode auto` without `--sandbox`, as it
    does beyond loopback: a paired account's messages come from outside the machine.
