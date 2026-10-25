- **Today, the morning briefing and settings in the web home** (`adk-surface` feature `web`;
  spec 3.3 §5.1, §10 K1, K3, K4): **Today** reads the kernel's "While you were away"
  (`GET /api/kernel/away`) — what was spent, whether quiet time was respected, how many
  decisions wait, and one line per event since the person last marked it seen, failures first,
  each with its status pill and **Details** that opens its record
  (`GET /api/kernel/away/lines/{seq}`). Opening the page moves nothing; **Mark as seen**
  (`POST /api/kernel/away/seen`) is the only thing that does, so each phone and the home keep
  their own place. Once quiet time ends, Threads shows the **morning briefing** — the start of
  the same page, once for each end of quiet time — with **See what happened** and **Not now**.
  **What your agents can do** reads and changes each area's quiet time, daily spending limit
  (with what was spent today) and mode through `/api/kernel/settings`, and shows each change
  as "Was 22:00–07:00. Now 23:00–06:00."; the header's status line follows the area's
  settings in force. **Where you can say yes from** pairs a chat account from the code it was
  sent, as `adk-rust pair accept <code>` does. A server without the kernel's ledger keeps the
  engine's briefing.
