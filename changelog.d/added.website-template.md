- **`cargo adk new <name> --template website`** (`cargo-adk`; spec 3.5 §7.17, §8.2): the site
  project `adk-website new` scaffolds — `site.toml`, the `paper` theme, a business draft
  charter with its own site id, capabilities, public terms, draft legal pages, `data/` and the
  holding page — with an edge binary: `cargo run` serves the release `adk-rust site publish`
  puts in `data/host` and `search_site` on the AWP 2.0 edge, `cargo run -- check` builds and
  runs the launch gate. The generated project ships a smoke test, and CI's template check
  builds and tests it.
