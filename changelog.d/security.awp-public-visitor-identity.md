- **AWP public visitors converse unauthenticated, under a keyed id** (`adk-server`,
  `adk-website`): a conversation from a caller with no verified credential no longer runs
  with a `RequestContext`, so `is_authenticated()` answers `false` and no standing approval,
  Auto mode, requested call or authenticated-only memory applies to it; the website Inbox
  runs strangers' messages the same way. The visitor id is `public-` and an HMAC-SHA256 of
  the caller's limit key under a key derived from `ConsentConfig::subject_key`, instead of an
  unkeyed SHA-256 digest of the peer address that hashing every IPv4 address reverses.
  Visitors behind one address still share an id and its `user:` state.
