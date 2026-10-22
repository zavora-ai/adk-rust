- **`adk-rust rules add` and `rules rm` are audited** (`adk-cli`, spec 3.1 §7.5, §9.3): the
  commands changed the standing-rule store without an audit record. They now grant and
  revoke through `HarnessAgent::grant_rule` and `revoke_rule`, which record
  `harness.rule.created` or `harness.rule.revoked` with `surface: cli` before the store write,
  as the REST routes record theirs with `surface: rest`; a change whose record cannot be
  written is not made.
