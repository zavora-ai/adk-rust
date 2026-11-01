- **Site decisions are approval requests** (`adk-harness`, `adk-site`, `adk-website`): a
  release under an approval chain, a charter signature and a job promotion are each one
  `ApprovalRequest` in the site's `ApprovalService` (`SiteRuntimeBuilder::approvals`), so the
  back office, the web desk, the kernel's inbox and chat cards decide one record and its
  votes survive a restart. `SiteApprovals::approve` and `reject` take the request id, its
  version and the person; `record_approval` is gone. A release vote binds to a
  `ReleaseFingerprint` of the change and its base release, from the new
  `adk_site::pipeline::change_digest` (`ApprovalRequest::change`). `SiteRuntimeBuilder::build`
  returns a `Result` and refuses in-memory stores under `require_durable`;
  `SiteRuntimeBuilder::governance` holds publishing while frozen. `SiteRuntime::sign_charter`
  takes the request and the signature only and commits with the request as the decision;
  `request_charter`, `charter_request`, `request_charter_signing`, `request_promotion` and
  `settle_charters` are new, and `restore_drafts` converts signing and promotion requests
  kept in the draft store, once. A served site keeps the requests in the kernel's approval
  service.
