- **`apply_patch`, multi-edit `edit_file` and `expect_sha256`** (`adk-devtools`,
  `adk-harness`): `apply_patch` takes the `*** Begin Patch` envelope, checks every hunk
  (exact, then ignoring trailing whitespace; never ambiguous or fuzzy) before writing,
  writes each file to a temporary sibling renamed into place, restores the files already
  written on any failure, keeps each line's ending, and reports `beforeSha256` and
  `afterSha256` per file. `edit_file { path, edits: [...] }` applies several replacements to
  one file together or not at all. `edit_file`, `write_file` and `apply_patch` take
  `expect_sha256` (`absent` for a new file), checked on the descriptor the write goes
  through. The harness declares all three edit tools for every model, classifies
  `apply_patch` as an edit judged on every path of its patch (a protected path anywhere is
  non-waivable, a `PathWithin` rule matches only when every path is within), runs
  `PostEdit` hooks once per file, and names the preferred multi-file tool per model family
  in the `<harness>` block (spec 3.1 §7.26, §13 Q12).
