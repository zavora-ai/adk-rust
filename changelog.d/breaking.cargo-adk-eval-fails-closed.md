- **`cargo adk eval` runs an agent binary over a line protocol** (`cargo-adk`): the
  command never ran an agent, so every case, JUnit report, and `--check-regression` run
  came out green. `--agent-cmd "<command>"` is now required: the CLI starts that
  process, writes one `{"case_id","turn","user_text","session_id"}` request per turn to
  its stdin, reads one `{"text","tool_calls":[{"name","args"}]}` or `{"error"}` response
  from its stdout, and scores the responses with `adk_eval::Evaluator`. `--criteria`
  takes an `EvaluationCriteria` JSON file, `--judge-model` builds an LLM judge from
  `GOOGLE_API_KEY`, `ANTHROPIC_API_KEY`, or `OPENAI_API_KEY`, and the command exits
  non-zero when a case fails, the agent misbehaves, or a score regresses. The ignored
  `--model` and `--concurrency` flags are removed.
