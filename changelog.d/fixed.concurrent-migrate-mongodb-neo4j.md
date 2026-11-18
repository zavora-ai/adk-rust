- **MongoDB and Neo4j `migrate()` is safe to run from several processes at once**
  (`adk-session`, `adk-memory`): replicas starting together on a new database no longer
  fail with a duplicate registry key (MongoDB `E11000`, Neo4j
  `ConstraintValidationFailed`) or `EquivalentSchemaRuleAlreadyExists`. Each version is
  recorded with an upsert or `MERGE`, a schema statement that loses a race to create the
  same rule succeeds, and the baseline check looks for v1's last index or constraint, so a
  `migrate` that finds another one half-way through v1 runs v1 itself.
