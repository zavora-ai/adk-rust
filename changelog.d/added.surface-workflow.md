- **The workflow observer** (`adk-surface`, feature `workflow`; spec 3.3 §12): `workflow::Workflows`
  reads a graph agent's run and step records, its forks' lineage and the effect log, and
  projects a session's runs, each node's status, each step's "What changed" (redacted, with the
  host's hidden keys and a size cap) and what a fork would do. With `web`,
  `web::workflow::router` serves them under `/api/surface/workflow` with its own OpenAPI
  document, and the developer panel's **Runs** tab draws the run graph, the steps, a step's
  detail and the fork dialog. A fork (`Workflows::fork`) starts a new session of the owner at a
  completed step, from the web home only, never while a decision waits, made and not started
  under Stop all, and is recorded as `surface.workflow.fork`. `Workflows` gives a governed
  node's card money, no **Always allow**, "Can't be undone" and "From the invoice-run workflow ·
  Open in Runs" as card hints, and `Workflows::approval_router` makes a node that spends money
  high risk on every surface. Example `scripted_workflow`.
- **`adk-rust graph steps` and `graph fork --set`** (`adk-cli`; spec 3.3 §12): `graph steps`
  prints each run, every step's node records and a fork's lineage; `graph fork --set
  key=<json>` forks with edits through `adk_graph::time_travel::fork_thread`, unchecked against
  the graph's channels and recorded so in the lineage. Every printed value has its control
  characters escaped, and `--db` opens the SQLite checkpoints in the default build.
