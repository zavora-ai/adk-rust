- **File tools open without following symlinks** (`adk-devtools`): on Unix, `read_file`,
  `edit_file` and `write_file` open the canonical target the containment check found without
  following a symlink on any component — `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`
  on Linux, a component-by-component `O_NOFOLLOW` walk relative to the parent's descriptor
  elsewhere — and `write_file` creates missing directories the same way. A file or directory
  swapped for a symlink after the check, such as by a concurrent `bash` command, fails the call
  instead of redirecting a read outside the root or a write to another workspace file.
  `read_file` and `edit_file` refuse a file with more than one hard link. Windows keeps the path
  checks only. Implements spec 3.1 §10 D4.
