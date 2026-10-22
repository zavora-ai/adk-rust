- **Kernel control plane** (`adk-kernel`, spec 3.2 §7.11): `ControlPlane` serves the
  kernel's routes under `/api/kernel` on an owner-only Unix socket that admits only the
  OS user running the kernel, and on HTTP with the caller's `RequestContextExtractor`
  or loopback peers; a non-loopback bind without one is refused. Routes for health, the
  process table with each process's live view, `spawn`, `kill`, `nice`, `suspend`,
  `resume`, server-sent events from a cursor, `attach` (transcript cursor, held calls,
  process), `freeze` and `unfreeze`; the approvals inbox is served on the socket too.
  Changes need the `operator` scope, and every action that changes the kernel or reads a
  session writes a `kernel.control` audit record with its subject and transport.
  `ControlClient` speaks it over the socket or HTTP, with `spawn`, `approvals` and
  `decide` for surfaces that send turns and answer held calls.
