- **Concurrent Neo4j appends keep every state delta** (`adk-session`): `Neo4jSessionService`
  read the app, user, and session state, merged the delta, and wrote the whole object
  back, so two concurrent appends dropped each other's keys. Each state key is now its
  own node property, written with `SET n += $delta` inside the append transaction, so an
  append writes only its own keys. Tiers that earlier releases stored as one JSON object
  are still read, and a key written since overrides the same key in that object.
