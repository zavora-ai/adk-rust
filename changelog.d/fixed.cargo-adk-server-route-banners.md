- **Server scaffolds name the routes they serve** (`cargo-adk`): a project with the `server`
  addon — the `a2a` (`a2a-server`), `production` and `chatbot` templates — logs the A2A v1 card
  at `/.well-known/agent-card.json`, the 0.3 card at `/.well-known/agent.json`, A2A v1 JSON-RPC
  at `/a2a/v1` and 0.3 JSON-RPC at `/a2a` and `/a2a/stream` when it starts with
  `RUST_LOG=info`. `cargo adk templates --json` no longer describes `api` as serving
  `/chat` and `/health`, which answer 404; its routes are `/api/health`, `/api/sessions` and
  `/api/run/{app}/{user}/{session}`.
