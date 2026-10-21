- **Encrypted harness store** (`adk-harness` feature `encrypted`, `adk-session`, `adk-core`;
  spec 3.1 §7.12, §8.1, I57): one `StoreKeyring` resolved once at startup from a
  `StoreKeySource` — an `adk-auth` secret provider, the OS keychain, or an environment
  variable — seals every payload of `harness.db`: sessions through `EncryptedSession`,
  held calls and decisions through the new `adk_session::EncryptedApprovalStore`, and the
  SQLite mode, rule, shell-ledger, goal and fact stores through `with_keyring`.
  Identifiers, timestamps and status columns stay plaintext for indexing; a database
  written under a key refuses to open without it or with the wrong one, never falling back
  to plaintext. `adk_session::envelope` exposes the `adk-enc:v1:` AES-256-GCM envelope,
  `EncryptionKey` gains `from_base64`, `to_base64` and `id`, `EncryptedSession` and
  `EncryptedApprovalStore` re-seal SQLite rows under a new key with `rewrap_sqlite`, and
  `StoreRotation` describes a rotation for the `harness.store.key_rotated` audit record.
  `ApprovalStore::record_pending_keyed` (provided; implemented by the in-memory and SQL
  stores) lets a wrapper record a held call under the fingerprint of the call as the model
  made it, since the stored arguments are sealed and the row key is a keyed digest.
