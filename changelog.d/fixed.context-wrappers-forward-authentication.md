- **Context wrappers forward `is_authenticated`** (`adk-agent`, `adk-tool`, `adk-graph`,
  `adk-realtime`): tools and callbacks inside an `LlmAgent`, workflow agents, teams,
  blackboards, CodeAct, agent-tool delegates, graph nodes and realtime agent tools reported
  an authenticated user as unauthenticated, because their contexts kept the trait's `false`
  default instead of asking the context they wrap.
