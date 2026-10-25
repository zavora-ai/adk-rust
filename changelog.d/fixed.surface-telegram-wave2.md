- **The Telegram thread tells the person what it did not take, and keeps its times and links
  right** (`adk-surface`, `adk-cli`; spec 3.3 §5.3, §8.1):
  - A paired person's message or press the kernel could not take is answered: "That message
    didn't reach your agents. Try sending it again." or "That answer wasn't recorded, so
    nothing was decided. Try again, or decide in the home."
  - An answer given in Telegram is recorded with where it was given before its confirmation is
    sent, so a slow Telegram or a stop no longer loses "You said yes in Telegram"; a quorum's
    earlier answers are not recorded as deciding.
  - Every time in the thread, "Decided already … at" and a waiting pairing's "before"
    included, is at `TelegramAdapterBuilder::utc_offset`; without it, times follow the host's
    time zone with daylight saving time.
  - `adk-rust up --ephemeral` shares how each answer was given between the thread and the home
    in memory, so the home says "You said yes in Telegram". Cards link to `TELEGRAM_HOME_URL`
    when set, and never to `0.0.0.0`. The home is served while cards of decisions that waited
    through a restart are sent, and cards keep reaching the thread after a refused token ends
    the polling. A second process reading the same bot (`409`) is logged once as an error.
