- **The `<harness>` block keeps what each safety mechanism covers** (`adk-harness`): a
  surface with a long command list, such as `adk-rust` built with `website`, filled the
  6 KiB block and cut the safety lines. The block now cuts the flags first, then the setup
  lines, then the commands, and keeps the class lines, the safety lines and the
  `harness_help` line. `harness_help` answers `classes`, `setup`, `safety` and `flags`
  with that section, the names the block gives the sections it cuts.
