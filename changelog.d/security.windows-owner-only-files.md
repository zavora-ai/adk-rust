- **Owner-only files on Windows** (`adk-harness`, `adk-cli`, spec 3.2 §10 W4): the files Unix
  creates with mode `0600` — the harness file stores, the CLI's `harness.db`, `adk-rust audit
  keygen`'s signing key and `adk-rust memory export` output — and the minted-credential helper
  inherited their directory's permissions on Windows. They are now created with a protected
  DACL whose single entry grants the current user full access, set by `CreateFileW` itself so
  the file never exists with inherited permissions. As with a Unix mode, an existing file keeps
  its own.
