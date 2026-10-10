- **SQL session backends filter events in the query** (`adk-session`): PostgreSQL and
  SQLite `get` apply `num_recent_events` and `after` in SQL instead of loading every
  event of the session and trimming the list.
