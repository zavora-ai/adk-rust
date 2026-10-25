- **`adk-rust up` runs as a Windows service again** (`adk-cli`): the service dispatcher's thread
  has the executable's 1 MiB stack, and a debug build of `up` — which since the harness manifest
  builds the command tree at start — overflowed it, so the service crashed before its pipe
  answered and restarted. The service's work now runs on a thread with the 8 MiB stack the
  other commands run on.
