- **`adk-rust` opens the full-screen terminal by default** (`adk-cli`, spec 3.3 §5.2): `tui`
  joins `adk-cli`'s default features, so a plain `adk-rust` opens the full screen on a capable
  terminal and inline otherwise. `--ui repl` keeps the line-by-line REPL, as `--voice` does;
  `--ui inline` and `--output json` are unchanged.
