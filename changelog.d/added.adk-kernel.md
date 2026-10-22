- **`adk-kernel`, the Agent Kernel** (new crate, Beta; spec 3.2 phase 1): a resident
  supervisor that keeps agents as durable processes over `DurableRunStore`, with one
  `DurableWorker` per queue class (`interactive`, `background`, `maintenance`), the
  `DurableScheduler`, wake sources, an event bus and a control plane.
  - `ProcessTable` lists, spawns, kills, suspends, resumes and requeues (`nice`) processes,
    each with a live `ProcessView`: age, spend today and over seven days, burn rate,
    projection, model and tool calls, the loop guard's verdict and a goal's progress. Spend
    is kept per process and day in the `ProcessStore` (in-memory, SQLite, PostgreSQL,
    Redis, with `process_store_conformance_tests!`).
  - A process spawned with a `WakeSet` sleeps `Waiting` with no lease and no model call
    until a cron tick, a file change, a signed webhook, a queue message, an approval
    decision, another process's end or a time wakes it; every event becomes one
    `kernel.wake` run keyed by the event, so a redelivery wakes nothing, and a standing
    `Trigger` spawns a process for an event no sleeper takes.
  - `Kernel::freeze` parks every running process `Waiting` on every replica sharing a
    `KernelConfig::mirrored_governance` store, and `unfreeze` resumes exactly those; the
    kernel is the `FreezeDelegate` of `POST /api/admin/freeze`.
  - `start`, `pause`, `resume`, `drain`, `stop` and `shutdown_on_signal`; `KernelEvent`s
    to subscribers with replay and to `AuditSink`; `control::router` with `ControlPlane`
    on a Unix socket and loopback HTTP, and `ControlClient`; `KernelConfig::clock` for a
    controlled clock.
