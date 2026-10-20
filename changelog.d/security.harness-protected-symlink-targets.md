- **Protected paths are decided on the target a write reaches** (`adk-harness`): a
  `write_file` or `edit_file` whose path resolves through a symlink inside the workspace
  to a protected file, such as `docs/notes.md -> ../AGENTS.md` or `cfg -> .git`, is the
  protected edit class (non-waivable approval in Ask and Auto, denied in ReadOnly and
  Plan), a `PathWithin` rule never covers it, and a run-wide `tool_approvals` entry for it
  is removed. A path through a dangling or looping symlink stays refused. New
  `approvals::matcher::is_protected_target` (spec 3.1 §9.2 I11, I29).
