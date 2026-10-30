- **"Always allow for this folder" covers every file write in the folder** (`adk-harness`,
  `adk-surface`, `adk-cli`): the rule was made for the one tool it was asked about, so after
  "Always allow" on an `edit_file` in `notes/` the next `write_file` there still asked. The
  folder rule now covers `write_file`, `edit_file` and an `apply_patch` that only adds or
  updates files, each pinned to its exact declaration (`OriginPin::ExactEach`), through the
  new `RulePredicate::WritesWithin`; `HarnessAgent::folder_suggestion` builds it. Writes
  outside the folder, `..` and symlink escapes, deletes, moves, the shell and other tools still
  ask. The button reads "Always allow for this folder (file writes anywhere in notes/, this
  session)", the home says "Your agents can write files within notes/, in one conversation."
  and "You allowed file writes in notes/ without asking.", and the `harness.rule.created`
  record keeps the covered tools as data (spec 3.3 §8.2).
