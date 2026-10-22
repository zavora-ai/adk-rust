- **Plans checkpoint in the backend in use** (`adk-harness`, `adk-cli`; spec 3.1 §7.11,
  §7.12, §13 Q5): `checkpointer_from_url` opens the plan and goal-graph checkpointer a store
  URL names (`sqlite://`, `postgres://`, `redis://`, `mongodb://`, `neo4j://`,
  `firestore://`, each under the feature of the same name) and
  `HarnessAgentBuilder::plan_checkpoint_store(url)` sets it; `SealedCheckpointer` seals each
  checkpoint's payload under the store key, and `rewrap_sqlite` re-seals a database's
  checkpoints on rotation. `adk-rust` checkpoints plans in the store its sessions are in —
  `harness.db`, or `serve --store` — so an approved plan runs in the next process;
  `--checkpointer <url>` and the user config's `[plan] checkpointer` name another, memory
  is used only with `--ephemeral`, the banner says where plans are kept, and
  `store status` and `store rotate-key` cover the checkpoints. The CLI's goals stay on the
  loop engine; `serve` runs them on the checkpointed graph.
