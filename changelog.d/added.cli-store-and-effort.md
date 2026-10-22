- **`adk-rust` sealed store and model posture** (`adk-cli`; spec 3.1 §7.12, §7.23, §8.1):
  `--store-key keychain:<entry>|secret:<provider>/<name>|env:<VAR>` and `[store] key` in the
  user config seal `harness.db` under one keyring; the first run offers to create a key in
  the OS keychain; `adk-rust store status` reports the key source, the key the database is
  sealed under and which stores are wrapped; `adk-rust store rotate-key` re-seals every
  store under a new key, retires the current one, seals rows written before any key, vacuums
  the file and reports `harness.store.key_rotated`. `--effort low|medium|high` and the
  `[effort]` table set the session's effort policy, which the harness applies to every model
  call (Gemini `thinking_level`, OpenAI `reasoning_effort`, Anthropic `output_config.effort`
  with adaptive thinking; members at `members`, the distiller at `distiller`), a Gemini model
  gets the runner's explicit context cache, `/effort` sets every mode and role to another
  level from the next call, `/cost` lists cached against uncached prompt tokens per turn, and
  the banner shows the effort and the store.
