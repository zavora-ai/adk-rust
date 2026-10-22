- **Standing rules over a served harness's REST API** (`adk-cli`, `adk-harness`, spec 3.1
  §7.5, §8.2): `adk-rust serve` and `HarnessLauncher::build_app` mount `GET /api/rules`,
  `GET /api/rules/suggestions/{approvalId}`, `POST /api/rules` (from a held call's
  suggestion or explicit) and `DELETE /api/rules/{id}` behind the same authentication as
  `/api/approvals`, for callers with the `operator` scope. A rule's subject is the
  authenticated caller, never a body field, and each grant and revocation is recorded as
  `harness.rule.created` or `harness.rule.revoked` with `surface: rest`.
  `HarnessAgent::rule_for_held_call`, `grant_rule` and `revoke_rule` give code that
  authenticates the person itself the same path.
