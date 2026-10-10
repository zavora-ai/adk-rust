- **`user.define_outcome` sends the documented body** (`adk-anthropic`, `managed-agents`):
  `UserEvent::DefineOutcome` carries `description`, `rubric` (`OutcomeRubric::Text` or
  `OutcomeRubric::File`), and an optional `max_iterations` instead of `criteria`, and
  `ManagedAgentsClient::define_outcome(session_id, description, rubric)` replaces
  `define_outcome(session_id, criteria)`. The API rejects `criteria` as an unknown field,
  so the previous shape never started an outcome. Pass former criteria as
  `OutcomeRubric::text(criteria)`.
