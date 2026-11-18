- **SQLite, MongoDB and Neo4j `migrate()` are safe to run from several processes at
  once** (`adk-session`, `adk-memory`): replicas starting together on a new database no
  longer fail with a duplicate registry key (MongoDB `E11000`, Neo4j
  `ConstraintValidationFailed`), `EquivalentSchemaRuleAlreadyExists`, or a SQLite step that
  another process already applied (`table … already exists`). The SQL runners
  (`migration::sqlite_runner`) run each SQLite step in a `BEGIN IMMEDIATE` transaction that
  re-reads the registry; MongoDB and Neo4j record each version with an upsert or `MERGE`,
  a schema statement that loses a race to create the same rule succeeds, and the baseline
  check looks for v1's last index or constraint, so a `migrate` that finds another one
  half-way through v1 runs v1 itself.
