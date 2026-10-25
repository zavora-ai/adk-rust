- **AWP 2.0 edge runtime** (`adk-awp`, spec 3.5 §7.2–§7.4, §7.9): `adk_awp::v2` serves
  `GET /awp/v2/manifest`, `/trust`, `/status`, `POST /awp/v2/capabilities/{name}` and
  `GET /awp/v2/calls/{id}` with 2.0 documents and error envelopes. `TierAssigner` assigns tiers
  from verified credentials only, failing closed to `public`; `claimed_agent` keeps
  self-declared headers as an analytics label. `CapabilityTool` exposes a capability as an
  `adk_core::Tool` with origin `awp:{domain}` and validates its input schema;
  `CapabilityRegistry` builds the manifest for a tier. `DirectExecutor` is the one governed
  path for every binding: visibility, sunset, schema, idempotency key, consent receipts, a
  recorded outcome per key (A-I7), and held calls answered `202` with `decide` and
  `expire_due`. Edge stores for subscriptions, consent receipts, status, held calls, leads and
  idempotency records ship with in-memory implementations and
  `edge_store_conformance_tests!`. `migrate_config` turns a `business.toml` into 2.0 site data
  and a draft charter. The 1.x routes and `AwpState` move behind the default `awp-v1` feature.
