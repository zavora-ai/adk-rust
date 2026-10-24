- **`adk-cli` builds without its default features** (`adk-cli`): `--no-default-features`,
  alone or with `--features gemini`, failed to compile because the `proc` command was
  dispatched without the `harness` feature, and with no provider compiled `create_model`
  left its arguments unread under `-D warnings`. Both sets now pass clippy and their tests,
  and the `feature-coverage` shard 2 builds them; a `-` in the features column of
  `scripts/feature-coverage-pairs.txt` names a set with no features.
