- **Vision egress allows by default** (`adk-vision`): `EgressConfig::default()` and an
  `[egress]` table with no `default` now `allow`, so with no `.adk/vision.toml` an image a
  remote backend is called with leaves as it is, and its release is still recorded. `ask` and
  `deny` remain available as the default and per rule, and hold or refuse exactly as before.
