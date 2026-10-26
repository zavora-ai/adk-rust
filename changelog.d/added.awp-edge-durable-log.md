- **Durable event log and beacon store at the edge** (`adk-server` `awp-sqlite`,
  `awp-postgres`, `awp-redis`): `SqliteEdgeStores`, `PostgresEdgeStores` and
  `RedisEdgeStores` implement `EventLog` and `BeaconStore`, and `edge_stores()` hands both to
  the edge (`EdgeStores::with_event_log`, `with_beacon`). `EventLog` gains provided methods
  for catalogue pages, the consent-reference index and pending deliveries (`PendingDelivery`,
  `claim_deliveries`); `EdgeEvents::resume_deliveries` and `AwpEdge::spawn_delivery_resumer`
  resume the retries a stopped process left, after `EventsConfig::delivery_lease`. The
  `edge_log_conformance_tests!` suite (`awp::conformance`) checks every backend (spec 3.5
  §7.5, §7.9, §7.12).
