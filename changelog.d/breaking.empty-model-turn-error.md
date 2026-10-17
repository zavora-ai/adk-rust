- **A model turn without an answer fails the run** (`adk-agent`): a final turn with no parts,
  only thinking, or only whitespace now ends the run with `model.empty_response` (its
  `details.metadata.finish_reason` names the finish reason) instead of a silent success, and
  `output_key` is not written. Responses a `before_model_callback` supplies are not checked.
