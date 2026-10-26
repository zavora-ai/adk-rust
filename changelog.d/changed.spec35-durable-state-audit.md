- **The edge reads amounts asynchronously** (`adk-awp`, `adk-server`, `adk-website`): the
  governed path decides `above_amount` holds with `CapabilityTool::amount`; `adk-website`'s
  order and payment handlers read an order's or quote's total from their store there, so
  their synchronous `call_amount` now answers the input's `amount` member.
- **A site runtime's audit trail also receives the pipeline's records** (`adk-harness`):
  `SiteRuntimeBuilder::audit` passes its trail to the runtime's pipeline, which writes the
  `site.content.*` and `site.release.*` records beside the runtime's own.
- **A served site is durable across a restart** (`adk-website`): `deploy::serve` keeps
  quotes, orders, bookings, the back office's drafts, the owner's alarms and the edge's
  rate-limit windows in `data/edge.db`, and every audit record in `data/audit.jsonl`, where
  before they were kept in memory or only logged.
