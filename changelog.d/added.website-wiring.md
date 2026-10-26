- **`adk-website`: the running site wired end to end** (spec 3.5 §7.8.3, §7.12, §7.17,
  D-11): `deploy::serve` uses the `adk-rust site` project layout with delegated key files and
  no root key, refuses to publish until the owner delegates the release key, keeps the
  kernel's stores in SQLite so a restart keeps charters, jobs, signals, approvals and paired
  devices, drives the `UpdateEngine` with its journal for `site batches` and `site feeds` and
  held changes as the owner's decision cards, runs experiments on their trigger with the
  measurement sources, mounts the Concierge over the edge's own public tools, and adds the
  updates to the weekly review; `SiteEdgeParts` takes the public agent, rate limits, an
  action ledger and an audit trail. `adk-harness`'s
  `SiteRuntime::shared_pipeline` shares the runtime's pipeline with the engine.
