- **Harness file memory** (`adk-harness`, feature `memory`; spec 3.1 §7.15):
  `HarnessAgentBuilder::memory(MemoryConfig)` adds `FileMemory` topic files with a derived
  `MEMORY.md` index, per user outside the workspace by default
  (`~/.adk/memory/<app>/<sha256(user)>/<project-slug>`) or in `<workspace>/.adk/memory`,
  with size limits. `memory_read` is a read; `memory_write` and `memory_delete` need a
  one-time approval in Ask and Auto, are denied in ReadOnly and Plan, are never covered by a
  standing rule, and record `harness.memory.written` with before and after digests. The
  index is the last part of the instruction, delimited as data.
