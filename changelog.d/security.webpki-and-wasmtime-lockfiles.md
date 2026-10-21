- **Certificate verification and Wasmtime lockfiles** (workspace and standalone examples): the
  workspace `Cargo.lock` moves to rustls-webpki 0.103.16 (RUSTSEC-2026-0382, distrusted
  intermediates accepted during certificate verification), webpki-roots and webpki-root-certs
  1.0.10 (RUSTSEC-2026-0381, RUSTSEC-2026-0383, distrusted emSign intermediates in the bundled
  roots), and wasmtime 48.0.7 with Cranelift 0.135.7 (RUSTSEC-2026-0378, RUSTSEC-2026-0379),
  and 143 standalone example lockfiles to the same rustls-webpki and webpki root releases.
  Lockfiles affect only builds of this repository and its examples; published crates keep their
  version requirements, so a downstream project gets these fixes by updating its own lockfile.
