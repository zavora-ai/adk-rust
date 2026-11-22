- **Artifact references in memories** (`adk-memory`): `MemoryEntry::with_artifact` adds a
  `Part::FileData` reference beside an entry's text, and `text::artifact_references` returns
  an entry's references — file data, and inline data or embedded resources that name their
  source URI, never their bytes. Search still matches text alone. The knowledge graph's
  episodic log keeps references in a column of its own (migration 3) instead of dropping them.
