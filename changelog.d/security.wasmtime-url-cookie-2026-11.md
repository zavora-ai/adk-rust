- **Wasmtime, `url` and `cookie` advisories** (`adk-sandbox`, feature `wasm`; workspace and
  standalone example lockfiles): `adk-sandbox` requires wasmtime and wasmtime-wasi 48.0.10,
  the first 48.x release with the fix for RUSTSEC-2026-0587 (a WASI guest reaches files
  outside its preopened directory on Windows through a drive-prefixed path) and for
  RUSTSEC-2026-0566 to RUSTSEC-2026-0573 (guest-triggered aborts, panics and unbounded host
  memory growth). The lockfiles move to `url` 2.5.9 (RUSTSEC-2026-0592, stack overflow in
  `Url::origin()` on nested `blob:` URLs) and `cookie` 0.18.3 (RUSTSEC-2026-0594, repeated
  private-cookie nonces after a fork). A downstream project gets the `url` and `cookie` fixes
  by updating its own lockfile.
