- **`adk-rust up` listens once its kernel is up** (`adk-cli`): the address is bound first, so
  a taken port still fails at once, but accepts connections only after the kernel and its
  control socket started. A command such as `adk-rust drivers --home DIR list`, run as soon as
  the address answers, no longer finds "no kernel runs at".
- **A follow-up just scheduled can be snoozed** (`adk-kernel`, `adk-surface`): a process a
  worker claimed only to park it on its wake set has not started, so snooze and
  `Agenda::schedule` move it instead of refusing "it has already started"; the new wake set
  is kept through the park. This also holds while a replica stopped in that moment still
  leases the run.
