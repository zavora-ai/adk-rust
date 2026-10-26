- **A sandboxed `git commit` succeeds** (`adk-harness`): the read-write sandbox kept all of
  `.git` read-only, so `git add` and `git commit` could not create `index.lock` and each needed
  the unsandboxed retry. Of a `.git` directory holding `hooks`, `config` and `info`, it now keeps
  only those three read-only — at the root, in nested repositories and in submodule gitdirs
  under `.git/modules` — and leaves the index, refs and objects writable. A `.git` missing one
  of them stays read-only whole, a `.git` file stays read-only, and the file tools keep all of
  `.git` protected.
