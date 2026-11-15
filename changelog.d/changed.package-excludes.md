- **Smaller published crates** (44 crates): `tests/`, `examples/`, and `benches/` no longer ship
  in the `.crate` files, nor `adk-gemini`'s 1.57 MB sample video, `test_data/`, `AGENTS.md`,
  and `.gitignore`, `adk-realtime`'s `AGENTS.md`, or `adk-computer-use`'s wire-contract
  fixtures. The 49 crates package to 10.1 MB instead of 13.8 MB, and `adk-gemini` to 145 KB
  instead of 1.41 MB. README and licence files still ship; run the test suites and examples
  from the repository.
