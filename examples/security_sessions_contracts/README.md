# Security and Sessions Contracts

Escalated payments, SSO audit failures, and the error contract of the artifact and session
backends. Every scenario drives the stack a deployment uses — `LlmAgentBuilder`, `Runner`,
the session, artifact, and approval services, `OpenAIClient`, the `adk-payments` toolset, and
the SSO `JwtValidator` — and asserts the behaviour a caller relies on.

A scripted OpenAI-compatible server on 127.0.0.1 plays the model, and a local issuer on
127.0.0.1 generates an ES256 key, serves its JWKS, and signs ID tokens. The Redis checks run
against `REDIS_URL`; the live scenario runs against OpenAI when `OPENAI_API_KEY` is set.

## Run

```bash
cargo run --manifest-path examples/security_sessions_contracts/Cargo.toml
REDIS_URL=redis://127.0.0.1:6379 cargo run --manifest-path examples/security_sessions_contracts/Cargo.toml -- --scenario session_identifier_errors_are_typed
```

The example reads `ADK_ENV_FILE`, then `./.env`, then the repository's `.env`; values in the first
file found replace shell exports.

## Scenarios

| Scenario | Setup | Asserts |
|----------|-------|---------|
| `escalated_payment_resumes_from_approval_store` | `payments_checkout_create` under a policy that reviews payments above $50; the scripted model buys a $750 desk; the approver decides the held call in `InMemoryApprovalStore`; three runs | Run 1 holds the call and the store lists it as pending; run 2 resumes it and the merchant sees one checkout; the approval is consumed; run 3 pays nothing |
| `live_escalated_payment_resumes` | The same flow against `gpt-5.6-terra` (`OPENAI_MODEL` overrides) | As above; skipped when the model does not call the tool |
| `sso_audit_failure_refuses_access` | `SsoAccessControl` with a `JwtValidator` over the local JWKS, once with `InMemoryAuditSink` and once with a sink whose store is down | A recorded grant is returned; an unrecorded grant is refused; a denial stays `AccessDenied` |
| `artifact_errors_are_typed` | A tool loading `quarterly-report.pdf` (absent) and `../../etc/passwd` through `ctx.artifacts()`, on `InMemoryArtifactService` and `FileArtifactService` | `NotFound` (`artifact.not_found`, `is_not_found()`) and `InvalidInput` (`artifact.invalid_name`), never `artifact.legacy` |
| `session_identifier_errors_are_typed` | `create` with an empty user ID, `get` with a NUL session ID, and `list` with a 513-byte app name on the in-memory, encrypted, SQLite, and Redis services, and `Runner::run_str` with an empty user ID | `session.invalid_identifier` (`Session`, `InvalidInput`) everywhere |

## Results

| Scenario | `main` (fa18640f) | This branch |
|----------|-------------------|-------------|
| `escalated_payment_resumes_from_approval_store` | FAIL — the store listed nothing; after approval run 2 held the call again and the merchant saw 0 checkouts | PASS |
| `live_escalated_payment_resumes` | FAIL — as the scripted scenario | PASS |
| `sso_audit_failure_refuses_access` | FAIL — the engineer was granted access with no audit record | PASS |
| `artifact_errors_are_typed` | FAIL — both failures were `Internal` / `artifact.legacy` on both backends | PASS |
| `session_identifier_errors_are_typed` | FAIL — every backend and the runner returned `config.legacy` in the `Server` component | PASS |
