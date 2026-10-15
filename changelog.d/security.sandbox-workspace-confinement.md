- **Workspace file operations stay inside the workspace** (`adk-sandbox`, `adk-devtools`):
  `LocalUnixSession` checked paths only as text, so `apply_patch` wrote any file its
  headers named, such as `--- ../victim.txt`, and `read_file`, `write_file`, `list_dir`,
  and the command working directory followed symlinks out of the workspace. Every path,
  including each file name in a patch header and each manifest entry, now resolves inside
  the workspace through symlinks, and a dangling symlink is refused. Writing a file with
  more than one hard link in either crate replaces it, keeping its permissions, instead of
  truncating it, so a link outside the workspace keeps its contents.
