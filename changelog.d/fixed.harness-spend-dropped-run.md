- **A harness run whose stream is dropped still counts its spend** (`adk-harness`): a turn
  that a person's denial or Ctrl-C ends stops the launcher polling the run; its model calls
  and tokens are now added to `HarnessRuntime::spend` when the stream is dropped. Before,
  only a stream polled to its end or to an error was counted, so `adk-rust ultracode`
  charged nothing for such a turn against `max_model_calls`.
