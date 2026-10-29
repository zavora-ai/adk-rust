- **Steering a running agent** (`adk-agent`): `LlmAgentBuilder::steering_source` takes a
  `SteeringSource`. At each safe point of the tool loop, after a step's tool results are in the
  history and before the next model request, the agent appends the user messages the source
  returns, so the next request and its `before_model` callbacks see them. The source cannot
  fail the turn; without one, nothing changes.
