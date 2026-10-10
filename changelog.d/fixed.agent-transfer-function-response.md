- **Transfers record a function response** (`adk-agent`): the event that carries
  `actions.transfer_to_agent` now contains a function response for the
  `transfer_to_agent` call and for every other call in the same model turn, which
  the transfer skips. A call already answered with an unknown-agent error is not
  answered again, so every call holds exactly one response. Histories that include
  a transfer no longer hold calls without responses, which the Anthropic API
  rejects with a 400 once an agent that reads the full history, such as the
  coordinator after a hand-back, sends them.
