- **Embedded Python builds outside the workspace** (`adk-code`, `adk-codeact-monty`): the
  `get-size2 0.10.1` pin that `monty`'s `ruff_python_ast 0.0.3` needs lived only in the workspace
  `Cargo.lock`, so a project depending on `adk-code` with `embedded-python`, on
  `adk-codeact-monty`, or on `adk-rust` with `code-embedded-python` resolved `get-size2 0.10.3`
  and failed to compile `ruff_python_ast` (E0277). `adk-code` now pins `get-size2 = "=0.10.1"`
  under `embedded-python`, so the constraint reaches dependents.
