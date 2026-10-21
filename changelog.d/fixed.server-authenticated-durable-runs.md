- **Background runs keep the caller's authentication** (`adk-core`, `adk-session`,
  `adk-server`, `adk-runner`): `NewRun::authenticated` and `RunRecord::authenticated`
  (`NewRun::with_authenticated_user`) record that a server authenticated the run's user, every
  run store persists it (the SQL stores add the `authenticated` column in migration 2), and
  `DurableWorker` runs such a run with a `RunConfig::request_context` for its user, so its tools
  answer `is_authenticated()` with `true`. `adk-server` marks the runs and cron jobs an
  authenticated caller submits; before, every durable run read as unauthenticated, so
  `require_authenticated_identity()` never held in one. `RunConfig::request_context` replaces
  the runner's request context for one run and must name the run's user.
