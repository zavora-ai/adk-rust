- **`cargo adk templates` and `cargo adk addons` list the same addons** (`cargo-adk`): each
  printed its own hard-coded list, and `templates` left out `docker`. Both now list the
  addons `cargo adk new` composes from, with the same descriptions, features and priorities.
