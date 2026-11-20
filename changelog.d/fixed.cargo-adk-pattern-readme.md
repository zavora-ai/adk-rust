- **Pattern projects' READMEs name the pattern** (`cargo-adk`): `cargo adk new --template a2a`
  (and every other enterprise pattern) writes a README whose template entry is the pattern,
  such as `a2a-server`, built on its base template, instead of `llm` alone.
  `codegen::generate_pattern_readme` builds it.
