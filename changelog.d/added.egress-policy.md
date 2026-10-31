- **Network egress policy** (`adk-core`): `egress::EgressPolicy` admits destinations by host
  (`api.example.com`, `*.example.com`, `*`), port and CIDR (`10.0.0.0/8:5432`); `decide()` is a
  pure check of a name, a resolved address or an IP literal, and `narrow()` intersects two
  policies so lower layers only tighten an operator's ceiling. Cloud metadata endpoints are
  always refused; other non-public addresses need a CIDR rule or `with_private_networks()`.
  `egress::GuardedClient` (feature `egress-client`) checks every request URL, every resolved
  address and every redirect hop, ignores proxy settings, and fails to build rather than fall
  back to an unguarded client; `egress::EgressResolver` is its resolver for crates with their
  own `reqwest` client. `WebhookDelivery`, AWP event delivery and Web Bot Auth directory fetches
  send through it. See `docs/official_docs/security/egress.md`. Refs #1281.
