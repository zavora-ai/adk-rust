- **The phase scenarios of agent surfaces as runnable examples** (spec 3.3 §12):
  `examples/surface_approve_tomorrow` ("Approve it tomorrow": a routine's email waits through
  quiet time and a `kill -9` of `adk-rust up`, is allowed once in Telegram and sent exactly
  once; the web home, the terminal and the record say "in Telegram"; a stranger gets a pairing
  code and starts nothing) and `examples/surface_morning_after` ("The morning after": two
  routines finish, one fails, a suggestion waits, a phone allows a call with its device token,
  the kernel restarts, and "While you were away" is read once with counts and spend that match
  the kernel's stores). Each runs on SQLite or PostgreSQL, scripted or live, and exits
  non-zero when a pass condition fails. See `docs/official_docs/surfaces/scenarios.md`.
