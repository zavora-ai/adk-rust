- **A durable audit recorder** (`adk-core`): `audit_log::FileAuditRecorder` appends each
  record to a JSON Lines file, numbers the records from 0 without a gap across processes and
  restarts, cuts a line a crash left unfinished, and reads the file back with
  `FileAuditRecorder::read` (spec 3.5 §9.3).
- **The pipeline's audit records and `site.release` span** (`adk-site`):
  `PublishingPipelineBuilder::audit` writes `site.content.{drafted, approved, scheduled,
  published, corrected, withdrawn}` and `site.release.{built, signed, published, refused}`;
  every release runs in a `site.release` span (`pipeline::RELEASE_SPAN`) carrying its
  sequence, source and items; `pipeline::audit::record_release_logged` and
  `record_release_published` record a release made outside the pipeline (spec 3.5 §7.24,
  §9.3).
- **Shared rate-limit windows at the edge** (`adk-server`, `awp`): `RateLimitStore`,
  `InMemoryRateLimitStore`, `EdgeLimits::store` with `admit_request` and `admit_call`; the
  SQLite, PostgreSQL and Redis edge stores implement it and pass
  `rate_limit_conformance_tests!`, so replicas and a restarted edge count a caller against one
  window (spec 3.5 §7.9).
- **Async capability amounts** (`adk-awp`): `CapabilityHandler::amount` and
  `CapabilityTool::amount` read a call's amount where the handler keeps it, such as a stored
  order; the edge decides `above_amount` holds with them (spec 3.5 §7.4).
- **Durable drafts** (`adk-harness`, `website`): `SiteDraftStore`, `InMemorySiteDraftStore`,
  `SqliteSiteDraftStore` (with `sqlite`), `SiteRuntimeBuilder::drafts` and
  `SiteRuntime::restore_drafts` keep the back office's content drafts, charter drafts, and
  charter signing and promotion requests across a restart (spec 3.5 §7.8.2).
- **Durable commerce, alarms and audit trail for a served site** (`adk-website`):
  `edge::commerce::{CommerceStore, InMemoryCommerceStore, SqliteCommerceStore,
  StoredCalendar}` and `SiteEdge::build_with_commerce` keep quotes, orders and bookings;
  `alarms::{AlarmStore, InMemoryAlarmStore, SqliteAlarmStore}` and
  `AlarmRouter::with_store` keep the open, deferred and waiting alarms; `PaymentHandler`
  writes `awp.payment.{created, settled, rejected}`; `KernelSignals` files each signal in a
  `kernel.signal` span; a served site keeps all of these in `data/edge.db` and its audit trail
  in `data/audit.jsonl` (spec 3.5 §7.5, §7.9, §7.21, §7.24, §9.3).
- **`awp.keys` audit records** (`adk-cli`, `website`): `adk-rust awp keys rotate` and
  `delegate` append `awp.keys.rotated` and `awp.keys.delegated` to the project's
  `data/audit.jsonl`, where `adk-rust site` writes the pipeline's records (spec 3.5 §9.3).
