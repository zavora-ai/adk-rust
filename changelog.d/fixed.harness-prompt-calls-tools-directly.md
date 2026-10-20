- **Harness prompts tell the model to call tools and let the harness ask** (`adk-harness`):
  the base prompt, the `coding` preset's prompt and the Ask and Auto mode reminders state
  that a call needing approval is put to the user by the harness, so the model calls the
  tool instead of asking for permission in prose or calling `exit_plan_mode` outside Plan
  mode. With `gpt-5.6-terra`, the coding preset in Ask mode now fixes a failing test instead
  of ending the turn with "Please approve". The prompts also state that `bash` returns
  stdout, stderr and the exit code and ask for plain commands without pipes or `2>&1`,
  which a standing rule cannot cover (spec 3.1 §7.2, §7.5, §7.19).
