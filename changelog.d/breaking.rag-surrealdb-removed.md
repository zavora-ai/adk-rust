- **The SurrealDB vector store is removed** (`adk-rag`): `SurrealVectorStore` and the `surrealdb`
  feature are gone, and `full` no longer enables them. Use `PgVectorStore`, `LanceDBVectorStore`,
  `QdrantVectorStore`, or `AgentRetrievalStore`, and re-ingest the source documents; a deployment
  that keeps SurrealDB implements `VectorStore` in its own code. The removal takes 110 packages
  out of the lockfile, among them `surrealdb` and `surrealdb-core` (RUSTSEC-2026-0453,
  RUSTSEC-2026-0454), `rkyv` 0.7 (RUSTSEC-2026-0235), `rsa` (RUSTSEC-2023-0071), and every
  Business Source License crate.
