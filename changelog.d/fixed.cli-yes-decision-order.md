- **`[approved by --yes]` follows its `[tool-call]` line** (`adk-cli`, spec 3.1 §8.1): a
  decision the prompter makes without asking — and the denial of a call no terminal could
  ask about — is printed through `ConsoleIo::print_for_call`, which holds it until the
  console's event reader has shown the call, so the lines are in order under a durable
  goal too, where the worker runs the goal while the console reads the session.
