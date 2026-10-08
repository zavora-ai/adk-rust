- **rustls upgraded to 0.23.45** (workspace and every standalone example): resolves
  RUSTSEC-2026-0285. No API change; the lockfiles move together so `cargo check --locked`
  keeps passing for each example.
