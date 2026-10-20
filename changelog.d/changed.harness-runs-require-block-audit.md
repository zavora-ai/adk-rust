- **Every harness run is audited in Block mode** (`adk-harness`, feature `auth`; spec 3.1
  §9.2 I39): a run without an audit trail gets the harness's (`HarnessAgentBuilder::audit_log`,
  else an `EphemeralAuditLog`), and a run whose trail is in `AuditFailureMode::Warn` is
  refused with `harness.audit_required`. `HarnessAgent::configure_runner` also installs the
  harness's audit log on the runner; a runner's own `audit_log` set afterwards takes precedence.
