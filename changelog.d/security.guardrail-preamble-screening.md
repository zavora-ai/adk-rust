- **Output guardrails screen text written beside a tool call** (`adk-agent`): the text of a
  model turn that also calls tools, or that the provider paused, reached the client and the
  session unscreened. Output guardrails now screen it before either sees it, through
  `Guardrail::validate_preamble`: a block ends the run and the turn's calls do not run, and a
  transform rewrites the text and keeps the calls. OpenAI Responses `commentary` messages, kept
  as native message parts, are screened in place too, and the event's
  `gcp.vertex.agent.llm_response` debug copy carries the screened content instead of what the
  guardrails replaced. `SchemaValidator` passes a preamble, and `ContentFilter` applies its
  blocked keywords and maximum length to one.
