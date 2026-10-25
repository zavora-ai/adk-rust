# Web Bot Auth

`adk-auth` (feature `web-bot-auth`) gives automated HTTP clients a verified identity. An agent
signs each request with an HTTP Message Signature (RFC 9421) and names, in the
`Signature-Agent` field, the origin that publishes its key. A site verifies the signature
against that origin's key directory and learns who sent the request, where a `User-Agent`
string such as `GPTBot` is only a claim.

The module implements the IETF Web Bot Auth working-group draft
[`draft-ietf-webbotauth-httpsig-protocol-00`](https://datatracker.ietf.org/doc/draft-ietf-webbotauth-httpsig-protocol/)
and the key directory of
[`draft-meunier-http-message-signatures-directory-05`](https://datatracker.ietf.org/doc/draft-meunier-http-message-signatures-directory/),
which the working-group draft absorbed, for Ed25519 keys.

| Part | Type |
|------|------|
| Verify a request | `HttpSignatureVerifier`, `VerifiedRequester` |
| Sign an outgoing request | `HttpSignatureSigner` |
| Resolve and cache key directories | `KeyDirectorySource`, `HttpKeyDirectorySource`, `InMemoryKeyDirectorySource`, `DirectoryCachePolicy` |
| Publish a directory | `KeyDirectory`, `DirectoryResponse` |
| Keys | `Ed25519SigningKey`, `jwk_thumbprint` |
| Signature base and structured fields | `RequestHead`, `SignatureInput`, `ComponentId`, `structured` |

## Setup

```toml
[dependencies]
adk-auth = { version = "3.0.0", features = ["web-bot-auth"] }
http = "1"
reqwest = "0.12"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Verify requests

```rust
use std::sync::Arc;

use adk_auth::web_bot_auth::{HttpKeyDirectorySource, HttpSignatureVerifier, WebBotAuthError};

fn verifier() -> Result<HttpSignatureVerifier, WebBotAuthError> {
    let directories = Arc::new(HttpKeyDirectorySource::new()?);
    Ok(HttpSignatureVerifier::builder(directories).build())
}

async fn identify<B>(verifier: &HttpSignatureVerifier, request: &http::Request<B>) -> String {
    match verifier.verify_request(request).await {
        Ok(Some(requester)) => requester.id,
        Ok(None) => "anonymous".to_string(),
        Err(error) if error.is_unverified() => format!("unverified: {error}"),
        Err(error) => format!("invalid: {error}"),
    }
}
```

`verify_request` reads an origin-form URI (`/path?query`) with the authority from `Host` or
the URI, and `https` as the scheme unless the builder's `default_scheme` says otherwise.

| Outcome | Result | When |
|---------|--------|------|
| Verified | `Ok(Some(VerifiedRequester))` | A signature tagged `web-bot-auth` verifies against a key its directory publishes |
| No identity | `Ok(None)` | No `Signature`/`Signature-Input`, or no signature tagged `web-bot-auth` |
| Unverified | `Err(e)` with `e.is_unverified()` | The key material was not available: directory unreachable or malformed, key not published, fetch refused, or a `Signature-Agent` discovery type other than `directory` |
| Invalid | `Err(e)` otherwise | A check failed: bad signature, expired, created in the future, missing component or parameter, test key, key outside its validity window |

None of the failure outcomes grants an identity. A request carrying several Web Bot Auth
signatures returns the first, in `Signature-Input` order, that verifies.

`VerifiedRequester` serialises with camelCase fields:

| Field | Value |
|-------|-------|
| `id` | `wba:{directory origin}#{key thumbprint}` |
| `directoryOrigin` | The ASCII-serialised origin of the key directory, such as `https://crawler.example` |
| `keyThumbprint` | The signing key's JWK SHA-256 thumbprint (RFC 7638, RFC 8037), equal to `keyid` |
| `created`, `expires` | The signature's validity window |

### What the verifier checks

| Check | Rule |
|-------|------|
| Parameters | `created`, `expires` and `keyid` present; `alg`, when present, is `ed25519`; `expires` not before `created` |
| Covered components | `@authority` or `@target-uri`, and the signature's `Signature-Agent` member (its only one, or the one keyed to its label); plus each `require_component` |
| Freshness | `created` no later than now plus the skew; `expires` no earlier than now minus the skew; `expires - created` within `max_signature_lifetime` |
| `Signature-Agent` | A String naming an http(s) origin with no path, query or fragment; dictionary form, or the legacy single String |
| Key | Selected by its computed thumbprint, never by `kid`, from the directory of the covered `Signature-Agent` origin; `nbf`/`exp` include now; not the RFC 9421 test key |
| Signature | Ed25519 over the RFC 9421 signature base recomputed from the request |

| Builder method | Default |
|----------------|---------|
| `max_skew` | 60 s |
| `max_signature_lifetime` | 24 h (`None` for no limit) |
| `require_component` | None beyond the draft's minimum |
| `default_scheme` | `https` |
| `allow_test_keys` | `false` |
| `cache_policy` | `DirectoryCachePolicy::default()` |
| `clock` | `SystemClock` |

## Sign outgoing requests

```rust
use std::sync::Arc;

use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner, WebBotAuthError};

async fn fetch_signed(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let jwk = std::env::var("CRAWLER_PRIVATE_JWK")?;
    let key = Arc::new(Ed25519SigningKey::from_jwk(&jwk)?);
    let signer = HttpSignatureSigner::builder(key, "https://crawler.example").build()?;

    let client = reqwest::Client::new();
    let mut request = client.get(url).build()?;
    signer.sign_reqwest(&mut request)?;
    Ok(client.execute(request).await?.text().await?)
}

fn new_key() -> Result<String, WebBotAuthError> {
    // Store the private JWK as a secret; it is the only copy of the key.
    Ok(Ed25519SigningKey::generate()?.to_private_jwk().to_string())
}
```

Each signature covers `@authority`, `@method`, `@path`, `@query` and the signer's
`Signature-Agent` member, and carries `created`, `keyid`, `alg="ed25519"`, `expires`, a random
`nonce` and `tag="web-bot-auth"`. `sign_request` signs an `http::Request` the same way.

| Builder method | Default |
|----------------|---------|
| `label` | `sig1` |
| `agent_member` | The label |
| `components` | `@authority`, `@method`, `@path`, `@query` |
| `lifetime` | 5 minutes |
| `nonce` | On |
| `clock` | `SystemClock` |

The signer adds its member to an existing `Signature-Agent` dictionary and its signature to
existing `Signature-Input` and `Signature` fields, so a second signer can cover the first
signature's `signature-input`, `signature` and `signature-agent` members (draft §5.2.2).

## Publish your key directory

```rust
use adk_auth::web_bot_auth::{DirectoryResponse, Ed25519SigningKey, KeyDirectory, WebBotAuthError};

/// The response for `/.well-known/http-message-signatures-directory` on crawler.example.
fn directory(key: &Ed25519SigningKey, now: i64) -> Result<DirectoryResponse, WebBotAuthError> {
    KeyDirectory::from_keys([key]).signed_response(&[key], "crawler.example", now, now + 7 * 24 * 60 * 60)
}
```

Serve `response.body` at `/.well-known/http-message-signatures-directory` with
`Content-Type: application/http-message-signatures-directory+json`, a `Cache-Control`
lifetime, and the `Content-Digest`, `Signature-Input` and `Signature` values. The signatures
prove possession of each key over `@authority;req` and `content-digest` (draft Appendix B.1);
the label is `binding` for one key and `binding0`, `binding1`, … for several.
`KeyDirectory::to_json` alone gives the document without the proof.

To rotate, publish the new key beside the old one, sign with the new key, and remove the old
key after the longest cache lifetime has passed.

## Directory fetching

`HttpKeyDirectorySource` fetches the directory a request names, so it follows the draft's
server-side request forgery guidance (§6.7):

| Limit | Default | Builder method |
|-------|---------|----------------|
| Scheme | `https` only | `egress_policy` |
| Addresses | Public only: the URL's host and every resolved address are checked; no proxy | `egress_policy` |
| Redirects | Not followed; any status other than `200 OK` fails | — |
| Media type | `application/http-message-signatures-directory+json` | — |
| Timeout | 5 s | `timeout` |
| Body size | 64 KiB | `max_bytes` |
| Keys | 64 | `max_keys` |

`EgressPolicy::allow_private_networks()` admits loopback, private addresses and plain http,
for tests and in-cluster directories. Directory entries that are not Ed25519 keys, carry
private material (`d`), have a `use` other than `sig` or an `alg` other than Ed25519 are
skipped. `nbf` and `exp` are Unix seconds; values of 10^11 and above are read as
milliseconds, which deployed directories publish.

## Directory cache

| `DirectoryCachePolicy` field | Default | Meaning |
|------------------------------|---------|---------|
| `min_ttl` | 60 s | Shortest caching; an unknown `keyid` refetches only once the entry is this old |
| `max_ttl` | 24 h | Longest caching |
| `default_ttl` | 5 min | Lifetime when the response declares none |
| `stale_if_error` | 1 h | How long a stale directory keeps verifying while refreshes fail |
| `negative_ttl` | 30 s | How long a failed fetch is remembered |
| `max_entries` | 10,000 | Origins held at once; the oldest is evicted beyond it |

The cache lifetime is `Cache-Control: max-age` (zero for `no-store` or `no-cache`), else
`Expires` minus `Date`, less `Age`. A directory that resolves replaces the cached copy, so a
removed key stops verifying; a failed fetch never evicts a cached copy (draft §6.10).
Concurrent requests that miss the cache for one origin share a single fetch.

## Interoperability

| Source | Result |
|--------|--------|
| RFC 9421 Appendix B.2.6 | Signature base and Ed25519 signature reproduce byte for byte |
| Draft Appendix E.2.1 | The signer reproduces `Signature-Agent`, `Signature-Input` and `Signature` byte for byte; the verifier accepts them |
| Draft Appendix E.2.2 | The legacy single-String `Signature-Agent` verifies |
| Draft Appendix E.2.3 | `signed_response` reproduces the body, `Content-Digest`, `Signature-Input` and `Signature` byte for byte |
| RFC 8037 Appendix A.3 | `jwk_thumbprint` reproduces the thumbprint |
| Cloudflare's research server (draft Appendix F) | The verifier fetches its directory and verifies a request signed for it; the server accepts this module's signature in the legacy `Signature-Agent` form; its directory proof verifies over the base this module reconstructs |

> **Note:** the Cloudflare research server implements the earlier architecture draft and
> accepts only the legacy single-String `Signature-Agent`. `HttpSignatureSigner` sends the
> dictionary form, which draft-00 §5.2.1 requires of signers.

The live check is an ignored test:
`cargo nextest run -p adk-auth --features web-bot-auth --run-ignored only -E 'test(live_)'`.

## Standards

- [RFC 9421](https://www.rfc-editor.org/rfc/rfc9421) HTTP Message Signatures
- [RFC 8941](https://www.rfc-editor.org/rfc/rfc8941) Structured Field Values for HTTP
- [RFC 7638](https://www.rfc-editor.org/rfc/rfc7638) JWK Thumbprint, [RFC 8037](https://www.rfc-editor.org/rfc/rfc8037) OKP keys
- [RFC 9111](https://www.rfc-editor.org/rfc/rfc9111) HTTP Caching, [RFC 9530](https://www.rfc-editor.org/rfc/rfc9530) Digest Fields
- [`draft-ietf-webbotauth-httpsig-protocol-00`](https://datatracker.ietf.org/doc/draft-ietf-webbotauth-httpsig-protocol/)
- [`draft-meunier-http-message-signatures-directory-05`](https://datatracker.ietf.org/doc/draft-meunier-http-message-signatures-directory/)
