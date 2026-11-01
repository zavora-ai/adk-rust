- **RAG subject export and erasure** (`adk-rag`): `RagSubjectData` adapts any `VectorStore` to
  `adk_core::subject::SubjectDataStore`, finding a person's chunks by the `adk_subject` metadata
  tag (`SubjectPartition::Tagged`, erased with `delete_where`) or by a scope per user
  (`SubjectPartition::ScopePerUser`, erased with `purge_scope`), so `SubjectRights` erasure
  reaches retrieval data.
