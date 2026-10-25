- **Kernel routes for the home's Today and "Agent Memory and Rules"** (`adk-kernel`,
  `adk-cli`; spec 3.3 §10 K3, K4, K5):
  - **Settings while the kernel runs**: `GET /api/kernel/settings[/{partition}]` and
    `POST /api/kernel/settings/{partition}/{quiet-time,spending-limit,mode}` read and change
    each area's quiet time, daily spending limit and mode. A change replaces the start value
    for its area, is recorded as `kernel.control` with `from` and `to`, and takes effect from
    the agents' next step: admission reads it at every claim, work it held for quiet time or
    the budget is woken to be decided again, and `adk-rust up` gives each harness a mode store
    that reads the area's mode.
  - **While you were away**: `GET /api/kernel/away`, `POST /api/kernel/away/seen` and
    `GET /api/kernel/away/lines/{seq}` read the `/proc` ledger after a cursor per device —
    counts, spend, whether quiet time was respected, one line per event linked to its ledger
    row — exact across restarts.
  - **Memory and record**: `GET /api/kernel/memory` lists facts with provenance ("You said
    this", "You confirmed this", "A guess") and `POST /api/kernel/memory/facts/{id}/{confirm,fix,forget}`
    and `POST /api/kernel/memory/undo` correct them through a `MemoryBook`;
    `GET /api/kernel/record` and `GET /api/kernel/record/check` read the latest actions and
    check the signed audit log as `adk-rust audit verify` does, through a `RecordBook`.
  - Reading needs the `operator` scope or a device's `read`; changing a setting or a memory
    needs the `operator` scope and no device token. Settings and cursors live in the new
    `ControlStore` (in-memory, SQLite on `harness.db`, PostgreSQL, Redis, with
    `control_store_conformance_tests!`); `ControlClient` has a typed method per route.
