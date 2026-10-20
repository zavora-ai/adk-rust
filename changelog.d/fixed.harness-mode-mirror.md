- **The `harness:mode` mirror shows the stored mode** (`adk-harness`): a harness run
  replaces any value an event writes under `harness:mode` with the mode store's, and
  re-asserts the mirror when a run starts after a client `stateDelta` or another writer
  changed it. A tool's `state_delta` or a client request no longer makes a UI that reads
  the mirror show a mode the policy is not applying; `Harness::mode` reads the store
  (spec 3.1 §6 D-3, §9.2 I1).
