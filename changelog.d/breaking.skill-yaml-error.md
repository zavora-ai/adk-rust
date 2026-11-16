- **`SkillError::Yaml` no longer exposes `serde_yaml`** (`adk-skill`): the variant is
  `Yaml { message: String }`. Skill frontmatter, `adk-site` front matter, `adk-server`'s
  `yaml-agent` definitions and `cargo adk` read and write YAML with `serde-saphyr` (pure Rust,
  maintained) instead of the deprecated `serde_yaml` and `serde_yaml_ng`, whose
  `unsafe-libyaml` is unmaintained.
