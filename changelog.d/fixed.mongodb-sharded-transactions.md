- **`MongoSessionService` uses transactions on sharded clusters** (`adk-session`): the
  `hello` check enabled transactions only for a replica set member (`setName`), so a
  connection through a `mongos` router (`msg: "isdbgrid"`) wrote each session's documents
  without a transaction, as on a standalone server. Both now count, through the same check
  `MongoRunStore` uses. A `hello` that fails is logged and still treated as standalone.
