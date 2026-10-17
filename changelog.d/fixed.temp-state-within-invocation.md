- **`temp:` state is visible for the rest of its invocation** (`adk-runner`): the runner's
  session keeps `temp:` keys, so a later tool, a transferred agent and a `{temp:key}`
  instruction read them; session services still drop them when they persist the event.
