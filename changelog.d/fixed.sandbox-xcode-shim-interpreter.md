- **`python3` from `/usr/bin` runs under Seatbelt** (`adk-sandbox`): under an OS enforcer the
  interpreter is found through the child's `PATH`, and on macOS `/usr/bin/python3` is an Xcode
  shim that loads `libxcrun` from the developer directory, so every Python execution exited
  with an `xcrun` error unless a standalone interpreter came first on `PATH`. When the program
  resolves to an Xcode shim in `/usr/bin`, `ProcessBackend` now runs the tool `xcrun --find`
  names and grants the active developer directory read-only for that execution.
