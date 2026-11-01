- **Memory and artifact export and erasure for subject rights** (`adk-memory`,
  `adk-artifact`): `MemoryService::export_user` lists a user's entries on every memory backend,
  and `ArtifactService::list_user` and `delete_user` cover every session and the `user:` scope on
  the in-memory, file, and GCS backends. `MemorySubjectData` and `ArtifactSubjectData` adapt them
  to `adk_core::subject::SubjectDataStore`, proven by the shared conformance suite.
