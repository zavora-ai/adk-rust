- **A dropped `FileWatchTrigger` subscription stops its watcher** (`adk-agent`, `ambient`):
  `subscribe` parked the thread holding the `notify` watcher forever, so every dropped
  subscription kept two threads watching its directory until the process exited.
