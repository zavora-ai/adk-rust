- **Fenced JSON passes `output_schema` validation** (`adk-agent`): one Markdown code fence
  around the whole reply is removed before validation, `output_key` stores the unwrapped JSON,
  and `extract_typed` unwraps it too, so a fenced answer no longer costs every correction retry
  and fails the run.
