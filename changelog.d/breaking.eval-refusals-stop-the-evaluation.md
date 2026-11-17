- **A budget, spend-cap or kill-switch refusal stops an evaluation instead of failing cases**
  (`adk-eval`): `LlmJudge` and `StructuredJudge` send their calls through the current
  `ModelCallScope` (`generate_metered`), so they count against a run's budget and spend ledger
  where one is current. A run or judge call refused by a budget, a spend ledger or a frozen kill
  switch returns the new `EvalError::Refused` from `Evaluator::evaluate_case` and
  `Evaluator::evaluate_test_file` rather than a failed case, and `EvalError` is now
  `#[non_exhaustive]`. Every other judge error still fails its criterion.

  Migration: add a wildcard arm to exhaustive `match`es on `EvalError`, and treat
  `EvalError::Refused` (or `EvalError::is_refusal`) as "not evaluated". A judge model wrapped in
  `MeteredLlm` and called under a current scope is metered twice; drop the wrapper and let the
  scope meter it.
