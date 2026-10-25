- **AWP 2.0 agent edge** (`adk-server`, feature `awp`, spec 3.5 §7.3, §7.4, §7.9, §7.10):
  `ServerBuilder::awp(AwpEdge)` mounts a site's edge through `add_root_routes` and attaches the
  server's kill switch. `EdgeTiers` assigns `public`, `known`, `verified` or `partner` from
  verified credentials only — `WebBotAuthTrustAssigner`, `OAuthClientCredentials` (tiers by
  scope), `MtlsCredentials`, `ApiKeyCredentials` and owner rules — refuses a failed credential
  with `unauthorized`, and keys rate limits on the verified identity, else a credential digest,
  else the peer address. Each capability is an A2A skill on a signed, tier-filtered Agent Card,
  an MCP tool and `POST /awp/v2/capabilities/{name}` (JSON or form-encoded); all three enter
  `GovernedExecutor`: `authorize_tool_call` under `CapabilityPolicy`, held calls answered `202`
  and decided through the owner's `ApprovalService`, the action ledger, and a full-body
  idempotency record, so a repeat returns the first outcome (A-I7). The public partition hosts
  a caller-supplied agent on A2A and `GET /awp/v2/chat` with only declared capabilities,
  `search_site`, `reply_in_thread` and `emit_signal` (`PublicPartitionPolicy`, A-I2).
  `EdgeMode::ReadOnly` and the kill switch answer every non-read-only capability `503` with the
  signed status (A-I12); `EdgeStatusControl` switches read-only and back.
