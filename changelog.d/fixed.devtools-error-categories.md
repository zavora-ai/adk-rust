- **A failed devtools call tells the model why** (`adk-devtools`): the errors a model can act
  on map to rejection categories with codes, so the model sees the reason instead of
  `tool.internal (tool.legacy)`: `devtools.not_read` (Conflict) for an edit before a read,
  `devtools.no_match` and `devtools.ambiguous` (InvalidInput), `devtools.path_escape`,
  `devtools.read_only` and `devtools.bash_disabled` (Forbidden), `devtools.not_found` for a
  missing file, and `devtools.invalid_argument` for bad arguments; a timeout keeps `Timeout`
  (`devtools.timeout`) and other I/O failures stay internal and hidden (`devtools.io`)
  (spec 3.1 §7.33 fix 6).
