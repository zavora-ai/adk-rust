- **`EventActions` has an `input_screened` field** (`adk-core`): a struct literal that names
  every field needs it or `..Default::default()`. It is omitted from serialized events when
  `false`.
- **A nested agent's input guardrails screen every user message** (`adk-runner`): an input
  guardrail on a sub-agent, workflow step, or transfer target screens each message before the
  runner saves it, so it also governs what the root and earlier steps see. It previously
  applied only when that agent ran. An agent that wraps another without listing it in
  `sub_agents()` overrides `Agent::screen_user_input` to delegate to it.
- **A resumed call that ends the turn rejects a message sent with the resume** (`adk-agent`):
  when a held call resumed with a decision sets `escalate` or `skip_summarization` and the run
  also brought a user message, the run fails with `agent.resume_ended_turn` instead of ending
  without answering it. The message stays saved, and the next run answers it.
