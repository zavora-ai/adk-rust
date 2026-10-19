- **`cargo adk eval` runs an agent binary over a line protocol** (`cargo-adk`): the command
  never ran an agent, so every case, JUnit report, and `--check-regression` run came out green.
  `--agent-cmd "<command>"` is now required: the CLI starts that process, writes one
  `{"case_id","turn","user_text","session_id"}` request per turn to its stdin, reads one
  `{"text","tool_calls":[{"name","args"}]}` or `{"error"}` response from its stdout, and scores
  the responses with `adk_eval::Evaluator`. `--criteria` takes an `EvaluationCriteria` JSON file
  (default: `tool_trajectory_score` 1.0 and `response_similarity` 0.8), `--judge-model` builds
  the LLM judge with the API key of the model's provider (`GOOGLE_API_KEY` or `GEMINI_API_KEY`,
  `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`), `--baseline` names the baseline file, and
  `--turn-timeout` (default 300 seconds) bounds each response. Before any case runs, the command
  refuses an eval set without cases, criteria without a threshold, judged criteria without
  `--judge-model`, and `--check-regression` without a baseline; `--save-baseline` refuses a run
  in which a case did not complete. It exits non-zero when a case fails or a score regresses.
  The ignored `--model` and `--concurrency` flags are removed.
