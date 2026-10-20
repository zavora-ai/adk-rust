- **`AuthorizingSecretService` is importable from `adk_auth::secrets`** (`adk-auth`): its
  rustdoc imported `adk_auth::secrets::{AuthorizingSecretService, SecretGrant}`, a path that
  did not exist, and the example was not compiled. `adk_auth::secrets` now re-exports
  `AuthorizingSecretService`, `SecretGrant`, `SecretAuditSink`, and `SecretAccessDecision`,
  and the example runs as a doctest. The `adk_auth::secrets::authorizing` path still works.
