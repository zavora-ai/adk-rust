- **Subject kinds, subject delegation, and per-agent principals** (`adk-core`, `adk-runner`,
  `adk-server`): `ActingConfig::subject_kind` (`Human` by default) names what the session's
  user is, and runs created by `DurableScheduler`, `WebhookIngress`, and `QueueIngress` act for
  a `Service` (`NewRun::with_service_subject`). `SubjectDelegationPolicy` and
  `StaticSubjectDelegation` let a run act for another subject, with the session's subject at
  the head of the delegation chain. `IdentitySource::principal_for` and
  `StaticIdentitySource::with_agent` map named agents to principals.
  `DurableWorkerConfig::with_allowed_principal`, `DurableWorker::with_audit`,
  `Runner::default_principal`, `Runner::recover_unanswered_calls_with_config`,
  `Runner::root_agent_name`, and `Runner::audit_trail` support durable identity checks.
