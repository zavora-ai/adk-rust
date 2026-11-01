- **Visitor tokens and subject rights routes** (`adk-auth`, `adk-server`):
  `adk_auth::VisitorTokens` signs and verifies visitor tokens with rotating keys and builds the
  `__Host-adk_visitor` cookie. `adk_server::subject_rights` serves export, erasure, and link for
  a signed-in caller; a link needs the visitor's token and a fresh sign-in, once. The AWP edge's
  export and erasure reach every store of its `SubjectRights` — the public partition's sessions,
  the beacon's visitor sets (`BeaconStore::forget_visitor`), and each store added with
  `AwpEdgeBuilder::subject_store` — and `ReceiptConsentStore` makes its receipts the framework's
  consent records.
