- **Snapshots and rewind** (`adk-harness`, `adk-cli`): the harness snapshots the whole
  workspace before each user turn and plan step — `.gitignore` honoured, `.git` never
  opened, build directories skipped, files over 8 MiB and excluded paths listed as not
  covered — into content-addressed objects beside `harness.db`, sealed under the store key
  when one is configured (spec 3.1 §7.27, I68). `Harness::rewind_plan` and
  `Harness::rewind` restore files, transcript and plan from a snapshot, showing conflicts,
  protected paths and the effects that are not undone first; `/rewind`, `/rewind N`,
  `/redo` and `adk-rust sessions rewind` do the same at the terminal. On in adk-cli
  (`--no-snapshots`, `[snapshots]`), off in the library until
  `HarnessAgentBuilder::snapshots`. `adk-rust ultracode` shares the walker
  (`adk_harness::snapshot::walk`).
