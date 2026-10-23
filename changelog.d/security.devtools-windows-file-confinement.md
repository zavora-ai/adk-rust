- **File tools refuse links swapped in after the check on Windows** (`adk-devtools`, spec 3.2
  §10 W6): `read_file`, `edit_file` and `write_file` read and wrote the checked path with plain
  `tokio::fs` calls on Windows, so a symlink or junction planted on the path after the check
  redirected the call, and hard-linked files were read. Each open is now verified on its
  handle: the target is opened with `FILE_FLAG_OPEN_REPARSE_POINT` and without truncation, a
  name-surrogate reparse point (a symlink or a junction) is refused, the handle's final path
  must be the checked target beneath the root's own final path, a file with more than one hard
  link is refused for reads and replaced on writes, and the file is truncated only after every
  check. `write_file` opens or creates each directory on the path the same way and holds it
  open until the write ends. In-place reparse points such as cloud placeholders and
  WOF-compressed files are not refused; they are reopened through their handle so their filter
  serves the content.
