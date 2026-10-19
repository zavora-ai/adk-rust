- **A dropped Neo4j session call no longer breaks the next one** (`adk-session`): a
  `Neo4jSessionService` call dropped mid-query, such as by a cancelled request, returned its
  connection to the neo4rs pool with a response unread, and the next call on that connection
  failed with `unexpected response for RUN`. Every call now runs to completion on its own
  task, as every Neo4j durable store call does.
