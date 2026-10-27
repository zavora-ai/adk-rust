- **Long-term facts on PostgreSQL** (`adk-harness`, `adk-cli`, spec 3.3 §12 phase 3):
  `PostgresFactStore` (feature `postgres` with `memory`) keeps facts, their embeddings, the
  distillation cursor and consolidation records in one database every replica shares, and
  passes `fact_store_conformance_tests!`. `adk-rust up --store postgres://…` keeps long-term
  memory there, so the overnight tidy-up, "What your agents remember" and its corrections run
  on the shared store as they do on `harness.db`.
