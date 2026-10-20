- **`adk-harness`, the governed harness agent** (new crate, Beta; spec 3.1 phase 1):
  `HarnessAgent` wraps an `LlmAgent` with the file, shell, todo and `exit_plan_mode`
  tools, and governs every call through `HarnessPolicy`, a `ToolPolicy` that applies the
  session's mode (`ReadOnly`, `Plan`, `Ask`, `Auto`) and the subject's standing rules.
  - Modes live in a harness-owned `ModeStore` (in-memory, file, SQLite); tightening
    applies at the next call, and loosening only through `Harness::set_mode`, which
    cancels held calls first, or an approved `exit_plan_mode`.
  - Standing rules ("always allow `cargo test`") are bound to the subject and to the
    tool's declaration digest, with `ShellPrefix` and `PathWithin` predicates; a
    redeclared tool records `harness.rule.stale` and asks again.
  - `PromptingConfirmationHandler` serializes prompts, caps them per turn, escapes
    control characters, and refuses a truncated call approved unexpanded.
  - `bash` runs on the host behind approval, checks the kill switch every 250 ms, keeps a
    per-session shell ledger, and refuses outside the harness policy; `AGENTS.md` files
    load verbatim as delimited, hashed, audited blocks after a workspace trust decision.
  - `adk_harness::coding::CodingAgent` keeps the 3.0 builder over the harness.
- **`harness_agent` example**: scripted scenarios against `HarnessAgent` and the 3.0
  `CodingAgent`, and live plan-mode and standing-rule scenarios on OpenAI, Anthropic and
  Gemini.
