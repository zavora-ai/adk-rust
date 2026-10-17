- **`session_integrity` example** (`examples/session_integrity`): drives `Runner` and the
  `adk-server` REST and A2A endpoints over the in-memory, encrypted, SQLite, PostgreSQL, and
  Redis backends to check rewind scoping and initial state, transient lookup failures,
  duplicate and racing creates, typed not-found errors, event order, U+0000 strings, and list
  order.
