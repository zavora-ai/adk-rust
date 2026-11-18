- **Google Pub/Sub and Eventarc triggers** (`adk-server`, feature `google-triggers`):
  `ServerBuilder::with_google_push` serves `POST /triggers/pubsub/{name}` for Pub/Sub push
  subscriptions and `POST /triggers/eventarc/{name}` for CloudEvents in binary or structured
  mode. `GoogleOidcVerifier` checks each delivery's Google-signed OIDC token — RS256 over
  Google's cached JWK set, issuer, audience, lifetime and a required service-account
  allowlist. A delivery is acknowledged only once its run is durably enqueued, keyed by
  `pubsub:{trigger}:{subscription}:{messageId}` or `eventarc:{trigger}:{source}:{id}`, so a
  redelivery runs nothing new; a frozen kill switch answers `503` so the source redelivers.
  `GoogleProvider::with_jwks_cache` (`adk-auth`) reads Google's keys from another cache.
