- **PostgreSQL stores strings containing U+0000** (`adk-session`): `JSONB` cannot hold U+0000,
  so a tool result or state value containing it aborted the run. Such strings are now stored
  in an escaped form and read back unchanged; strings without U+0000 are stored as before.
