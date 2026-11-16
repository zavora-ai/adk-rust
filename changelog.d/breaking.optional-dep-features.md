- **Optional dependencies are no longer features** (`adk-agent`, `adk-artifact`, `adk-auth`,
  `adk-deploy`, `adk-eval`, `adk-guardrail`, `adk-memory`, `adk-session`, `adk-surface`,
  `adk-telemetry`, `adk-tool`): their features name optional dependencies with `dep:`, so the
  implicit features Cargo created for them (`adk-session/tokio`, `adk-auth/jsonwebtoken`,
  `adk-guardrail/jsonschema`, `adk-tool/reqwest`, …) are gone. Enable the feature that uses
  the dependency (`encrypted-session`, `sso`, `schema`, `http-transport`, …); the migration
  guide lists all 40.
