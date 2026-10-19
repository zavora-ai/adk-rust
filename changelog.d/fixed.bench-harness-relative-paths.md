- **`adk-bench` harness config has no machine-specific paths** (`adk-bench`):
  `harnesses/external-frameworks.json` named its two Python harnesses by absolute paths on one
  developer's machine; it now names them relative to the repository root, where the documented
  `cargo adk bench --external-config` command runs.
