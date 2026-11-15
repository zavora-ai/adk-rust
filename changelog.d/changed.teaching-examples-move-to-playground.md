- **Teaching examples move to adk-playground** (`examples/`): 100 standalone example crates leave
  this repository for [adk-playground](https://github.com/zavora-ai/adk-playground), and the
  README, crate READMEs and official docs name them there. `examples/` keeps the crates CI runs,
  the spec scenarios and the release-validation crates. The `examples-compile` job and the
  `pre-push` examples gate run two shards instead of four.
