- **Extend a standing rule** (`adk-harness`, `adk-cli`, spec 3.3 §8.1): `POST
  /api/rules/{id}/extend` with `{"expiresAt": …}` moves the end of one of the caller's rules to a
  later time, recorded as `harness.rule.extended` with the old and new end before the store
  write (`HarnessAgent::extend_rule`). `StandingRuleStore::replace` changes a stored rule only
  while it is still stored, in one step on every store of `adk-harness`, so an extension never
  brings back a rule revoked meanwhile.
