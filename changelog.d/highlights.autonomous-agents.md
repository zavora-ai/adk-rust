- **Autonomous agents.** 3.0 builds agents that pursue a goal over many steps, wake themselves,
  and keep working while nobody watches, inside limits you set. [Build an autonomous
  agent](docs/official_docs/autonomy/index.md) takes one from a scaffold to running on its own.
- **Governed.** Every tool call from every agent type passes one path
  (`adk_core::authorize_tool_call`): tool policies with default-deny, approvals that hold a call
  across runs and bind to its fingerprint, run budgets and a spend ledger with daily caps, a
  kill switch that freezes every replica, OS sandboxes (Seatbelt, bubblewrap, a Windows
  restricted token), and a signed audit log that `adk-rust audit verify` checks.
- **Durable.** Lease-fenced durable runs resume from their checkpoint after a crash; background
  runs, cron schedules and signed webhook wake-ups enqueue them; a held call survives a restart
  and runs once when approved; the action ledger keeps a non-idempotent call from running twice.
  The run, lease, schedule, governance, approval, spend, action and audit stores run on every
  session backend.
- **Always on, observable and steerable.** `adk-harness` is a governed agent with modes,
  standing approvals, a sandboxed shell, plans and durable goals; `adk-kernel` runs it as
  processes that sleep until a schedule, a file, a webhook, a queue or an approval wakes them
  (`adk-rust up`, `ps`, `top`, `freeze`); `adk-surface` attaches the web home, the terminal, a
  phone and chat to the same running state, and a message reaches a running turn
  (`Runner::steer`).
- **Secure by default.** The server binds `127.0.0.1` and refuses unlisted hosts and
  cross-origin requests; see the [2.x to 3.0 migration
  guide](docs/official_docs/migration/2.x-to-3.0.md) for every breaking change below.
