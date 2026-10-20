- **bubblewrap always starts a new session** (`adk-sandbox`): `LinuxEnforcer` passed
  `--new-session` only when the policy denied process spawning, so a sandboxed shell —
  which needs spawning — kept the caller's controlling terminal and could inject input
  into it with `TIOCSTI`. Every bubblewrap command now runs in a new session with no
  controlling terminal. Spec 3.1 §10 S2.
