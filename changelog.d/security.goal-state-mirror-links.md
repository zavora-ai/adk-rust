- **`adk-rust goal --state` never writes through a link** (`adk-cli`): the mirror was
  written to `<path>.tmp`, following a link an agent could place there in its workspace, and
  so overwrote a file outside the sandbox as the operator. It is now written to a new file of
  an unpredictable name and renamed into place, and the write is refused when a link inside
  the workspace is on the way to it or its directory resolves elsewhere than when the goal
  started.
