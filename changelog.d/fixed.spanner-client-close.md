- **Spanner tools close their client on every exit** (`adk-tool`): a failed query, a
  failed transaction or a row read error leaked the client and its session pool. Missing
  arguments are rejected before a client is created.
