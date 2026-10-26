- **AWP edge events, consent, status and stores** (`adk-server`, features `awp`, `awp-sqlite`,
  `awp-postgres`, `awp-redis`, spec 3.5 §7.5, §7.9, §7.12, A-I8): Standard Webhooks
  subscriptions at `/awp/v2/events`, owned by the Web Bot Auth or OAuth requester that created
  them, with public-`https`-only URLs, `AWP-Version: 2.0` deliveries, retries after 1 s to 2 h,
  suspension after six failures with a final `subscription.suspended`, 72-hour replay, and one
  `catalog.changed` per release with pages at `/awp/v2/events/changes/{seq}/{page}`
  (`EdgeEvents::announce_catalog` takes an `adk-site` announcement). Consent at
  `/awp/v2/consent`: receipts signed by the edge's `receipt` key for a pseudonymous subject,
  revocation that rejects pending calls relying on the receipt and sends `consent.revoked`,
  export and erasure authenticated by the subject's session or signed link. Status transitions
  publish `site.status.changed`. `POST /awp/v2/beacon` counts page views and events per day with
  no cookies, addresses or identifiers without a `persistent_profile` receipt, honouring Global
  Privacy Control; `AwpEdge::beacon()` reads the counts for the KPI tracker.
  `SqliteEdgeStores`, `PostgresEdgeStores` and `RedisEdgeStores` implement the six edge store
  traits and pass their conformance suite.
