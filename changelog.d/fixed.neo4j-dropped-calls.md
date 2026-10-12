- **Neo4j stores survive dropped calls** (`adk-session`): every Neo4j store call — run,
  lease, schedule, and governance stores, the spend and action ledgers, the approval and
  audit log stores, and the session service — runs to completion on its own task. A call
  dropped mid-query (the durable worker drops its in-flight heartbeat when an attempt
  ends) returned its connection to the neo4rs pool with a response unread, and the next
  call on it failed with `unexpected response for RUN` — a run's outcome write among them.
