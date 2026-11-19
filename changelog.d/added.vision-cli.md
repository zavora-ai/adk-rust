- **`adk-rust vision`** (`adk-cli`, features `vision` and `vision-local`): `models list`,
  `fetch`, `verify` and `accept` over the built-in manifest and your own, with licence
  acceptance recorded only from a terminal; `run detect|segment|read|describe <image>` on the
  device first, a remote backend only through the egress gate of `.adk/vision.toml`, with a
  held release approved by its exact digest at the terminal or with `--approve`; output as a
  table, JSON or a `RegionSet`, and `--annotate`; and `egress log`, every release and refusal
  of the project. See `docs/official_docs/vision/cli.md`.
