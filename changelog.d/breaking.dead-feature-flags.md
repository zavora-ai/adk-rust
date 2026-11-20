- **Feature flags that gated nothing are removed** (`adk-graph`, `adk-audio`): `adk-graph`'s
  `action-db-mongo`, `action-db-redis`, and `action-code`, and `adk-audio`'s `vad`, compiled no
  code of their own. Drop them from `features = [...]`: the MongoDB and Redis placeholders build
  under `action-db`, the code node under `action`, and `VadProcessor` under every feature set.
  `adk-graph/action-full` and `adk-audio/desktop-audio` no longer list them. `adk-rust`'s
  `action-code` is removed as well; use `action`.
