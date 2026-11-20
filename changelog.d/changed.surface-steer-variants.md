- **`Command` and `CommandOutcome` have steer variants** (`adk-surface`): `Command::Steer` and
  `CommandOutcome::Steered`. A `match` that names every variant needs an arm for each; the
  generated `api.ts` and `openapi.json` carry `steer` and `steered`.
