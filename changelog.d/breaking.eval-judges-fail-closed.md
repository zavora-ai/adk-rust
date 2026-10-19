- **LLM-judged criteria fail closed** (`adk-eval`): a judge reply without a valid `SCORE:` line
  (a finite number from 0.0 to 1.0) is an error instead of a default score of 1.0 for safety and
  hallucination or 0.0 for semantic and rubric scoring. Safety, hallucination, and semantic
  replies must also carry their `SAFE: YES/NO`, `HALLUCINATION_FREE: YES/NO`, or
  `EQUIVALENT: YES/NO/PARTIAL` line, so a `SemanticMatchConfig::custom_prompt` must ask for the
  `EQUIVALENT:` line as well as `SCORE:`. `SAFE: NO`, `HALLUCINATION_FREE: NO`, and
  `EQUIVALENT: NO` fail the criterion whatever the score. `semantic_match_score`,
  `rubric_quality_score`, `safety_score`, and `hallucination_score` fail the case with a stated
  reason when no LLM judge is configured, the agent produced no text, or no rubrics are set,
  instead of being skipped. A structured judge `fail` verdict or judge error fails the case, also
  when `collect_turn_details` is off. An error from the agent's event stream fails the case
  instead of being dropped, and cost and trace analysis use the evaluated turns' events instead
  of a second agent run.
- **Judge settings are applied, and `temperature` and `max_tokens` are options** (`adk-eval`):
  every judge request carries `LlmJudgeConfig`'s temperature and output limit, which were never
  sent. `LlmJudgeConfig::max_tokens` is `Option<usize>` and `temperature` is `Option<f64>`, both
  `None` by default: the provider's output limit applies, so thinking models are not truncated,
  and the request carries no temperature. `StructuredJudgeConfig::temperature` is `Option<f64>`
  and defaults to `None` instead of `0.0`, which Claude 5 models and OpenAI reasoning models
  reject. Wrap an explicit value in `Some`.
