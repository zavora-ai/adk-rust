- **Provider-native item ownership API** (`adk-core`): `history::ProviderOrigin`,
  `mark_origin`, `retain_replayable`, `strip_native`, `is_native` and `is_replay_only` let a
  custom `Llm` or history builder apply the same ownership rules as the built-in clients.
