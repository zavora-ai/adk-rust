- **Anthropic requests leave out blank text and empty turns** (`adk-model`): an assistant turn
  with only whitespace, or with no block the API accepts, was sent as a whitespace text block,
  which the API rejects, so every later request on the session failed with HTTP 400. Empty and
  whitespace text is left out, a turn left with no blocks is dropped, and the turns around it
  merge.
