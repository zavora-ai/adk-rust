- **Secret files are excluded from the dev tools** (`adk-devtools`): `Workspace::exclude_globs`
  sets file patterns that `grep` and `glob` skip and `read_file` refuses with
  `DevToolError::Excluded` (an `AdkError` with category `Forbidden` and code
  `devtools.path_excluded`), so a harness can require approval before reading one. A new
  workspace excludes `DEFAULT_EXCLUDE_GLOBS` — `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`,
  `id_ed25519*`, `*.p12`, `*.pfx` — matched without regard to case against the relative path
  and each of its components, and against the target of a symlink for `read_file`;
  `exclude_globs([])` turns exclusion off. `Workspace::is_excluded` checks a path. Spec 3.1
  §10 D5.
