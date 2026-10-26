- **Fork with edits and lineage** (`adk-graph`, `time-travel`): `TimeTravelHandle::fork`
  and `time_travel::fork_thread` branch a new thread from a checkpoint by id with
  channels overwritten, checked against the graph's channels or, without a schema,
  limited to existing keys. A fork records its root, parent, parent checkpoint and
  edits; refusals are `ForkError`s with stable codes (spec 3.3 §12).
