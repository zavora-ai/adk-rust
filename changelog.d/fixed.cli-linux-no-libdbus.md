- **`cargo install adk-cli` builds on Linux without `libdbus-1-dev`** (`adk-cli`,
  `adk-harness`): the OS keychain on Linux, FreeBSD and OpenBSD reaches the Secret Service
  through keyring's pure-Rust zbus backend (`async-secret-service`, `async-io`) instead of
  libdbus, so neither the build nor the binary needs the system D-Bus library.
