- **Eval schema gains vision fields** (`adk-eval`, `adk-bench`): `Turn` has
  `expected_vision`, `TurnResult` has `actual_vision`, `EvaluationCriteria` has `vision`,
  `schema::Part` has an `Image` variant, and `BenchError` has a `Vision` variant. Struct
  literals add the new fields (`expected_vision: None`, `actual_vision: None`, or
  `..Default::default()` for `EvaluationCriteria`), and exhaustive matches on `Part` add the
  `Image` arm. Eval files without images parse unchanged.
