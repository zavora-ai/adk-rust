- **Session archive** (`adk-session`, `adk-runner`, `adk-server`, `adk-cli`): `SessionService`
  gains `archive`, `unarchive`, `list_active`, `list_archived` and `export_session`, default
  methods every backend runs, proven by a shared archive contract. An archived session keeps
  its events and state under an `adk:archived` mark, leaves `list_active`, the REST session list
  (`?archived=true` lists it) and `adk-rust sessions list`, and `Runner` refuses its turns with
  `session.archived` until it is unarchived. `export_session` writes a portable JSON
  `SessionBundle` (`adk.session.bundle/v1`). `adk-rust sessions archive|unarchive|export` and
  `list --archived` drive it from the command line.
