- **AWP 2.0 signing, verification and conformance** (`adk-awp`, spec 3.5 §7.2, §7.7, D-11,
  A-I5, A-I9): the `signing` feature signs documents with a detached JWS (EdDSA) over their JCS
  form without `proof` through `DocumentSigner` and `Ed25519DocumentSigner`, issues root-signed
  delegations with scopes and windows, revocations and root endorsements, checks the
  `_awp-root` DNS anchor through `DnsTxtResolver`, signs and verifies A2A Agent Cards, writes the
  site log's C2SP tiles and signed checkpoint, and builds `StaticBundle` (the signed contract,
  `keys.json` and public terms). The `verify` feature adds `Verifier`, the library behind
  `adk-rust awp verify`: `GET`-only, fourteen named steps from the contract to sampled page
  digests, per-domain pins (`FilePinStore`), and a posture and readiness report; the
  conformance kit `conformance::run` for the `static`, `static+edge` and `dynamic` profiles; the
  document rules `conformance::rules`; and the AWP 2.0 test vectors.
