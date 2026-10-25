- **The web home says what happened, and who can say yes, without guessing** (`adk-surface`):
  - An approval reads "You allowed Finance to go ahead.", not "… once", since the record does
    not tell **Allow once** from **Always allow**; Stop all and Resume all lines carry a pill.
  - The thread head and the status line show the mode the conversation shown runs in
    (`GET /surface/status?session=`), not the agent's default.
  - Paired chat accounts read as their name and account ("Telegram: Ana (4242)"), **Pair**
    confirms the account the kernel paired, and a failed read of who can say yes says so and
    is read again.
  - The daily limit editor reads `5,50` as $5.50, not $550, and refuses ambiguous amounts; the
    quiet time editor says a change replaces every window the area has.
  - **Details** on a Today line tells its record in plain words instead of linking to the raw
    route, which answered `401` behind `--auth-token`; a stale briefing error no longer stays
    above the kernel's page.
  - Without a kernel ledger (`adk-rust serve`), Today says what agents do is not recorded
    there instead of "Nothing happened while you were away.": `Briefing::recorded` and the
    provided `Source::keeps_ledger` say whether a source keeps the ledger.
