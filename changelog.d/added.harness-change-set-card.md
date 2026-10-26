- **Change sets: one review per batch of edits** (`adk-core`, `adk-agent`, `adk-harness`,
  `adk-cli`): `ToolConfirmationHandler::batch_started(&[BatchCall])`, a provided no-op that
  `LlmAgent` calls once per model turn after the batch's budget reservation, lets the
  harness's prompting handler show two or more held `write_file`, `edit_file` and
  `apply_patch` calls of one turn as one card — a unified diff per file with its before and
  after sha256 and the change set's digest — and decide them with one answer (approve all,
  approve the selected files, deny all with a reason, or edit in `$VISUAL`). Each approved
  call runs with `expect_sha256` bound to the content the person saw; protected paths are
  asked about on their own; edits a standing rule allows are counted, not held;
  `harness.changeset.decided` records the decision. `ChangeSet::compose` is public for
  surfaces that group held edits themselves (spec 3.1 §7.26).
