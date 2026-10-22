- **Shared mode and rule stores for harness replicas** (`adk-harness` features `postgres`,
  `redis`, `adk-cli`; spec 3.1 §7.4, §7.5, §8.2, §13 Q2): `PostgresModeStore`,
  `PostgresStandingRuleStore`, `RedisModeStore` and `RedisStandingRuleStore` keep the SQLite
  stores' keys and semantics on a backend every replica shares. `rules_for` answers from
  rules loaded at the app's rule version, which every grant and revocation increments in the
  same transaction or script, so a revocation through one replica is refused by every replica
  from its next call. The PostgreSQL stores migrate under advisory locks; the Redis keys are
  namespaced per app with the app as the hash tag (`with_namespace` separates deployments).
  `shared_store_conformance_tests!` runs the mode and rule cases over two instances of one
  backend, and the rule store suite gains a Deny-beats-Allow and expiry case.
  `adk-rust serve --store postgres://…|redis://…` (features `postgres`, `redis`) keeps
  sessions, approvals, durable runs, modes, standing rules and plan checkpoints in that
  backend instead of `harness.db`.
