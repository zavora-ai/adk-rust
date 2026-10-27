- **AWP 2.0 Web Bot Auth vectors in `adk-auth`** (spec 3.5 §7.3, §11 "Interoperability"):
  `tests/web_bot_auth_vectors.rs` checks the protocol repository's nine Web Bot Auth vectors —
  RFC 9421 B.2.6, the draft's E.2.1, AWP manifest and capability requests, and the tampered,
  expired, not-yet-valid, uncovered `Signature-Agent` and unsigned cases — with
  `HttpSignatureVerifier`, reproduces each signature from its key, and holds the files to the
  protocol's `SHA256SUMS`.
