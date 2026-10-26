- **External generators on Windows** (`adk-site`, `gen-*`): `ExternalGenerator::with_restricted_token`
  runs Zola, Hugo, Astro and Build Awesome under `adk-sandbox`'s restricted token in a Job
  Object; the token confines writes and leaves the network open, so Windows builds run only by
  this opt-in. Nightly CI runs the real generators under bubblewrap and the restricted token,
  and the host suites against MinIO, Azurite and an SSH server (spec 3.5 §7.8, §11).
