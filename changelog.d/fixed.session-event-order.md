- **Events with equal timestamps keep their append order** (`adk-session`): Redis scores each
  event one above the highest score in its sorted set and orders by timestamp after reading,
  instead of scoring by millisecond, which reordered a call and its response stored in the
  same millisecond. PostgreSQL (migration 2, `events.seq`), MongoDB (migration 2,
  `events.seq`), Neo4j, and Firestore order equal timestamps by a per-session append sequence
  instead of the random event ID. Events written by earlier releases keep their order.
- **Redis lists sessions newest first** (`adk-session`): a sorted set scored by update time
  (`sessions_idx:<app>:<user>:updated`) backs `list`, so pages are stable and ordered as on the
  other backends; sessions created by earlier releases are added to it on first `list`. Every
  backend now breaks equal update times by session ID, descending.
