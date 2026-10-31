- **Subject registries, consent stores, and the session adapter** (`adk-session`): every session
  backend — SQLite, PostgreSQL, Redis, MongoDB, Neo4j, and Firestore — ships a
  `SubjectRegistry` and a `ConsentStore` that pass the `adk-core` conformance suites, and
  `SessionSubjectData` exports and erases a subject's sessions, events, and `user:` tier
  through any `SessionService` for `SubjectRights`.
