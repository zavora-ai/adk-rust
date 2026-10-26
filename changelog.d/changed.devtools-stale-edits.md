- **An edit of a file changed since it was read is refused** (`adk-devtools`): reads and
  writes record the file's sha256 per session and agent, and `edit_file` without
  `expect_sha256` requires the file to still have it, answering `devtools.stale`
  (`Conflict`, shown to the model) and writing nothing otherwise, so one team member no
  longer overwrites another's change unnoticed. The records sit behind the new
  `ReadLedger` trait (`InMemoryReadLedger` by default, `Workspace::with_read_ledger`); the
  harness refreshes the record after a `PostEdit` hook rewrites a file (spec 3.1 §7.26,
  §9.2 I66).
