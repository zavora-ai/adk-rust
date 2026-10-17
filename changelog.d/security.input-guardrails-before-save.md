- **Input guardrails screen a user message before it is saved** (`adk-runner`, `adk-agent`,
  `adk-core`): the runner saved the raw message before `LlmAgent` ran its input guardrails, so
  a blocked message stayed in the session and reached the model in later runs, including an
  empty resume after an approval, and a redacted message reached every later model request
  unredacted. The runner now screens each message with `Agent::screen_user_input` on its root,
  which applies the input guardrails of every agent in the tree, before saving it: a block
  fails the run with nothing saved, and a transformation such as PII redaction is saved in its
  transformed form. The saved message carries `EventActions::input_screened`. A saved message
  without the marker, from an earlier release or appended to the session directly, is screened
  in the run's view before any model sees it, so a durable worker or managed runtime that
  recovers a turn checks it again and fails the run when it is blocked.
