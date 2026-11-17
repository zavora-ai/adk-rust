- **Per-call metering for composite models** (`adk-core`): `Llm::generate_content_metered`
  lets a model that sends one request to several providers meter each provider call against
  the run's `ModelCallScope`; the default meters the call as one, as before
  (`ModelCallScope::generate_one_call`).
