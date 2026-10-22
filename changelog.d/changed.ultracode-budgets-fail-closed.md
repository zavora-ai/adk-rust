- **`ultracode` budgets every step and fails closed** (`adk-cli`): the implement and revise
  turns run under a `RunBudget` of `implement_model_calls` (default 100), each reviewer under
  `reviewer_model_calls` (default 20), and the run under `max_model_calls` (default 400): a
  review stage or a revise starts only when the rest of the budget covers it, else the run
  ends as `budgetExhausted`. A reviewer's verdict is its last `VERDICT:` line, so a reply
  that mentions `VERDICT: approve` and ends with `VERDICT: changes` requests changes; a
  missing verdict, a failed reviewer and a reply without a `VERDICT` line are not approvals.
  Before, any mention of `VERDICT: approve` approved, and a missing verdict counted as an
  approval. `--max-rounds` is at most 5.
