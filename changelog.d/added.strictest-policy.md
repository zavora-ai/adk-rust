- **`StrictestPolicy` composes tool policies** (`adk-core`): `StrictestPolicy::new(layers)`
  and `.layer(policy)` ask every layer through `evaluate_call` and return the strictest
  decision (`Deny` over `RequireApproval` over `Allow`, the earlier reason on a tie). Its
  `policy_ref()` joins the named layers' references with `+`, and `contains_ref(name)`
  detects a layer already present, so a `configure_run` hook adds its policy to a run's
  existing one once instead of replacing it.
