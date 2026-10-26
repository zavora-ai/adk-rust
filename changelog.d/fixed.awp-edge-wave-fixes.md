- **Erasure reaches leads** (`adk-server` `awp`): `POST /awp/v2/consent/erase` deletes the
  leads that reference the subject's receipts through the default `LeadsData`, which erased
  none (spec 3.5 §7.5).
- **Anonymous A2A message ids are the visitor's own** (`adk-server` `awp`): a retried message
  id from an anonymous caller returned the task of any other anonymous caller that used the
  same id, its message and answer included; message ids are now scoped to the caller's limit
  key.
- **A stateless MCP endpoint takes no JSON-RPC id as an idempotency key** (`adk-server` `awp`):
  without a session the id is not unique, so callers sharing ids were answered from each
  other's records; only the `_meta` key identifies a call there (AWP 2.0 §7.7).
- **The served edge keys anonymous limits on the peer address** (`adk-website`):
  `deploy::serve` and `serve_agency` now pass the peer address to the edge, so anonymous
  visitors no longer share one rate limit and one conversation user.
- **The AWP test vectors fail the same way on every retry** (`adk-awp`): the vector test writes
  drifted bytes for review under the target directory, and rewrites the committed copies only
  under `AWP_UPDATE_VECTORS=1`, so a retry no longer passes on the copy the first try wrote.
