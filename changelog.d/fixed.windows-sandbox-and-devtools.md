- **Windows: the Rust sandbox links again, Docker policy accepts rooted mounts, and the
  devtools `glob` tool finds files** (`adk-code`, `adk-devtools`): `rustc` is pinned to
  `rust-lld` as in `adk-sandbox`, so Git's GNU `link.exe` on PATH no longer intercepts the
  link step; bind-mount containment checks for a rooted rather than an absolute path, so
  Linux-container mounts such as `/srv/data` validate on a Windows host; and `glob` walks
  the workspace instead of expanding a canonical root whose `\\?\` prefix the glob crate
  read as a wildcard.
