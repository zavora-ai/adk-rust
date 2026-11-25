- **Image redaction is opt-in** (`adk-harness`, `adk-cli`): the harness vision pack and
  `adk-rust vision` no longer add `faces` and `personal_text` to every rule set; the
  `redact` list of `.adk/vision.toml` decides, empty by default. A listed class still refuses
  the release when no redactor covers it, and the pack still installs its default redactors
  (feature `vision-local`) for listed classes. `adk-rust vision` now installs its on-device
  redactors for the classes the rules list.
