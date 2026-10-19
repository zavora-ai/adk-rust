- **Console launchers print each tool call once** (`adk-runner`, `adk-cli`): a function call
  streamed by the model arrives in a partial event and again in the complete event, and the
  `Launcher` consoles, `adk-cli`'s console, and its `run` and `ultra` commands printed both.
  They now print it from the complete event. `StreamPrinter::handle_event` renders one event
  this way.
