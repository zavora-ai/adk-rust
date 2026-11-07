- **The A2A v1 handlers refuse requests without `A2A-Version`** (`adk-server`): A2A §3.6.2 reads
  a missing or empty version as 0.3, but the v1 handlers answered such requests with v1
  payloads. `v1::version::SUPPORTED_VERSIONS` is now `["1.0"]`, so a request without the header
  or with `0.3` gets `VersionNotSupportedError`. On JSON-RPC that error is now JSON-RPC error
  `-32009` with the request's `id`, instead of HTTP 400 with a REST-style body. REST keeps HTTP
  400 with the AIP-193 body. The version can also be sent as the `A2A-Version` query parameter,
  and the patch number is ignored, so `1.0.1` is served as `1.0`. Agent card routes are not
  version-checked.

  Migration: send `A2A-Version: 1.0`. The adk-rust, adk-python and adk-go clients already do.
