- **Autonomous agents.** 3.0 builds agents that pursue a goal over many steps, wake themselves,
  and keep working while nobody watches, inside limits you set. [Build an autonomous
  agent](docs/official_docs/autonomy/index.md) takes one from a scaffold to running on its own;
  [What's new in 3.0](docs/official_docs/whats-new-3.0.md) lists every area the release adds.
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
- **Runs a website.** AWP 2.0 gives a site a signed contract, signed releases and an agent
  edge beside the static pages; `adk-website` runs the site with a team of agents inside its
  owner's charter (`adk-rust website new`, `cargo adk new --template website`). [Spin up an
  AWP website](docs/official_docs/deployment/website-quickstart.md).
- **Sees, locally.** `adk-vision` detects, segments and reads on the device, with model
  manifests pinned by revision and SHA-256, and sends an image to a cloud model only through
  an egress gate (`vision`, in `full`; `vision-local` for the on-device backends). [Vision:
  getting started](docs/official_docs/vision/getting-started.md).
- **Secure by default.** The server binds `127.0.0.1` and refuses unlisted hosts and
  cross-origin requests; see the [2.x to 3.0 migration
  guide](docs/official_docs/migration/2.x-to-3.0.md) for every breaking change below.
