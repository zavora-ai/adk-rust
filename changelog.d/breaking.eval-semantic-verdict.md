- **`EQUIVALENT: NO` fails semantic matching** (`adk-eval`): a judge verdict of
  `EQUIVALENT: NO` fails the `semantic_match` criterion whatever its score, as `SAFE: NO`
  and `HALLUCINATION_FREE: NO` already do. A judge reply without an
  `EQUIVALENT: YES/NO/PARTIAL` line is a judge error, so a
  `SemanticMatchConfig::custom_prompt` must ask for that line as well as `SCORE:`.
