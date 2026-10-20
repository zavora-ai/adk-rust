- **`edit_file`'s read-before-edit check is per session and agent** (`adk-devtools`): clones
  of a `Workspace` shared one record of read files, so a `read_file` in one session or by one
  team member let another session or agent edit the file without reading it. Reads are now
  recorded per (app, user, session, agent) from the tool call's context, keeping the 1,024
  most recently used records; calls whose context names no session share one record as
  before. Spec 3.1 §10 D2.
