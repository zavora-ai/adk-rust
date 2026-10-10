- **LLM-judged criteria fail closed** (`adk-eval`): a judge reply without a valid `SCORE:`
  line (a finite number from 0.0 to 1.0) is an error instead of a default score of 1.0
  for safety and hallucination or 0.0 for semantic and rubric scoring, and safety and
  hallucination replies must also carry their `SAFE:` / `HALLUCINATION_FREE:` line.
  `SAFE: NO` and `HALLUCINATION_FREE: NO` fail the criterion regardless of score.
  `semantic_match_score`, `rubric_quality_score`, `safety_score`, and
  `hallucination_score` fail the case with a stated reason when no LLM judge is
  configured, the agent produced no text, or no rubrics are set, instead of being
  skipped. A structured judge `fail` verdict or judge error fails the case, also when
  `collect_turn_details` is off. An error from the agent's event stream fails the case
  instead of being dropped, and cost and trace analysis use the evaluated turns' events
  instead of a second agent run.
- **`LlmJudgeConfig` is applied, and `max_tokens` is `Option<usize>`** (`adk-eval`):
  judge requests carry the configured temperature and output limit. `max_tokens`
  defaults to `None`, the provider default, so thinking models are not truncated; wrap
  an explicit limit in `Some`.
