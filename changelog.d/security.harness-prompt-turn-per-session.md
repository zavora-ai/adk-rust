- **Prompt limits hold per session** (`adk-harness`): `PromptingConfirmationHandler` keeps
  its turn state — "deny all remaining" and the per-turn prompt cap — per session instead
  of one state for every session, so another session prompting no longer resets a
  person's "deny all remaining" or their cap mid-turn. Prompts stay serialized across the
  handler (spec 3.1 §7.5, threat I-3).
