- **Harness identity and credentials** (`adk-harness`, feature `auth`; spec 3.1 §7.14):
  `HarnessAgentBuilder::rbac(RbacMap)` requires a permission per tool class through
  `adk-auth`'s `ScopeGuard` (and `ProtectedTool` with an `AccessControl`), and a denial's
  error code names the permission (`harness.rbac.denied:harness:shell`).
  - `credentials(Arc<dyn CredentialBroker>)` and `shell_credential(ShellCredential)` mint a
    subject-scoped token per `bash` call, hand it to `git` through `GIT_ASKPASS` and a
    credential helper that exists only for the call, and revoke it afterwards.
  - `require_authenticated_identity()` makes standing rules and Auto inert for a run whose
    user the server did not authenticate (§9.2 I6).
  - `audit_log(Arc<dyn AuditRecorder>)` sets the harness's audit trail; `EphemeralAuditLog`
    is the signed, in-memory default.
