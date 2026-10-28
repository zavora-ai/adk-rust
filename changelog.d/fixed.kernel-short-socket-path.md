- **`adk-rust up --home` with a long path starts** (`adk-kernel`): `<home>/kernel.sock` longer
  than a Unix socket address holds (103 bytes on macOS, 107 on Linux) stopped the kernel from
  starting. The kernel now serves such a socket at `adk-rust-<tag>/kernel.sock` under the
  user's own temp directory, the tag derived from the resolved home, and every client — the
  attached terminal and the CLI commands — derives the same path from the home. The directory is
  created `0700`; the kernel and its clients refuse one that is a symlink, belongs to another
  user, or is open to other users. A path too long even there fails with an error naming the
  limit, both paths and the fix. A home whose socket fits keeps `<home>/kernel.sock`; Windows is
  unchanged.
