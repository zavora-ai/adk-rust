//! Forge push signatures for the webhook ingress (spec 3.5 §7.8.2 "Files and git").
//!
//! A [`WebhookHook`](super::WebhookHook) authenticates its deliveries with Standard Webhooks
//! by default; [`WebhookHook::forge`](super::WebhookHook::forge) takes a forge's own push
//! signature instead, so a repository host that cannot sign Standard Webhooks still reaches
//! the same ingress, idempotency and kill switch. No forge is preferred: each scheme is one
//! variant of [`ForgeSignature`].
//!
//! | Scheme | Authenticity | Delivery id |
//! |---|---|---|
//! | [`ForgeSignature::GitHub`] | `X-Hub-Signature-256: sha256=<hex>`, HMAC-SHA256 of the raw body keyed with the secret, compared in constant time | `X-GitHub-Delivery` |
//! | [`ForgeSignature::GitLab`] | `X-Gitlab-Token` equal to the secret token, compared in constant time | `X-Gitlab-Event-UUID`, else `Idempotency-Key` |
//!
//! Neither forge signs a timestamp, so a captured delivery verifies again later: the
//! ingress keys each run by the delivery id, so a redelivery is taken once, and a consumer
//! that acts on a delivery's content (such as a commit) deduplicates by that content too.

use std::sync::Arc;

use adk_core::webhook::{VerifiedWebhook, WebhookError};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// GitHub's signature header.
pub const GITHUB_SIGNATURE_HEADER: &str = "x-hub-signature-256";
/// GitHub's delivery id header.
pub const GITHUB_DELIVERY_HEADER: &str = "x-github-delivery";
/// GitLab's secret token header.
pub const GITLAB_TOKEN_HEADER: &str = "x-gitlab-token";
/// GitLab's event id header.
pub const GITLAB_EVENT_HEADER: &str = "x-gitlab-event-uuid";
/// GitLab's idempotency key header, sent on retries of one event.
pub const GITLAB_IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// A forge's shared secret: the HMAC key GitHub signs with, or the token GitLab sends.
///
/// # Example
///
/// ```rust
/// use adk_server::webhooks::ForgeSecret;
///
/// let secret = ForgeSecret::new("a shared secret").unwrap();
/// assert!(format!("{secret:?}").contains("REDACTED"));
/// assert!(ForgeSecret::new("  ").is_err());
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ForgeSecret {
    key: Arc<[u8]>,
}

impl std::fmt::Debug for ForgeSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForgeSecret").field("key", &"[REDACTED]").finish()
    }
}

impl ForgeSecret {
    /// The secret as the forge's settings hold it, surrounding whitespace removed.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookError::InvalidSecret`] when the secret is empty.
    pub fn new(secret: impl AsRef<str>) -> Result<Self, WebhookError> {
        let secret = secret.as_ref().trim();
        if secret.is_empty() {
            return Err(WebhookError::InvalidSecret("the forge secret is empty".to_string()));
        }
        Ok(Self { key: secret.as_bytes().into() })
    }
}

/// How a forge signs its push deliveries (spec 3.5 §7.8.2 "Files and git").
///
/// # Example
///
/// ```rust
/// use adk_server::webhooks::{ForgeSecret, ForgeSignature};
///
/// let github = ForgeSignature::GitHub(ForgeSecret::new("It's a Secret to Everybody").unwrap());
/// let body = b"Hello, World!";
/// // GitHub's documented example of `X-Hub-Signature-256`.
/// let signature = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
/// let headers = [("x-hub-signature-256", signature), ("x-github-delivery", "72d3162e")];
/// let lookup = |name: &str| headers.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
/// assert_eq!(github.verify(lookup, body).unwrap().id, "72d3162e");
/// assert!(github.verify(lookup, b"Hello, World?").is_err());
/// ```
#[derive(Clone, Debug)]
pub enum ForgeSignature {
    /// GitHub: `X-Hub-Signature-256`, HMAC-SHA256 of the raw body.
    GitHub(ForgeSecret),
    /// GitLab: `X-Gitlab-Token`, the secret token itself.
    GitLab(ForgeSecret),
}

impl ForgeSignature {
    /// The scheme's name: `github` or `gitlab`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::GitHub(_) => "github",
            Self::GitLab(_) => "gitlab",
        }
    }

    /// Checks a delivery's signature and returns its delivery id; the timestamp is the time
    /// of receipt, since neither forge signs one.
    ///
    /// `lookup` returns a header's value by lowercase name; `body` is the raw request body.
    ///
    /// # Errors
    ///
    /// [`WebhookError::MissingHeader`] without the signature or delivery header,
    /// [`WebhookError::MalformedHeader`] for a signature that is not `sha256=<hex>` or an
    /// empty or oversized delivery id, and [`WebhookError::SignatureMismatch`] when the
    /// signature or token does not match.
    pub fn verify<'a>(
        &self,
        lookup: impl Fn(&'static str) -> Option<&'a str>,
        body: &[u8],
    ) -> Result<VerifiedWebhook, WebhookError> {
        let id = match self {
            Self::GitHub(secret) => {
                let header = lookup(GITHUB_SIGNATURE_HEADER)
                    .ok_or(WebhookError::MissingHeader(GITHUB_SIGNATURE_HEADER))?;
                let signature = header
                    .trim()
                    .strip_prefix("sha256=")
                    .and_then(|hex| hex::decode(hex).ok())
                    .ok_or_else(|| WebhookError::MalformedHeader {
                        header: GITHUB_SIGNATURE_HEADER,
                        reason: "not `sha256=<hex>`".to_string(),
                    })?;
                // HMAC accepts keys of any length, and `ForgeSecret::new` refuses an empty one.
                let mut mac = Hmac::<Sha256>::new_from_slice(&secret.key)
                    .map_err(|_| WebhookError::SignatureMismatch)?;
                mac.update(body);
                // `verify_slice` compares in constant time.
                mac.verify_slice(&signature).map_err(|_| WebhookError::SignatureMismatch)?;
                lookup(GITHUB_DELIVERY_HEADER)
                    .ok_or(WebhookError::MissingHeader(GITHUB_DELIVERY_HEADER))?
            }
            Self::GitLab(secret) => {
                let token = lookup(GITLAB_TOKEN_HEADER)
                    .ok_or(WebhookError::MissingHeader(GITLAB_TOKEN_HEADER))?;
                if !same_secret(token.trim().as_bytes(), &secret.key) {
                    return Err(WebhookError::SignatureMismatch);
                }
                lookup(GITLAB_EVENT_HEADER)
                    .or_else(|| lookup(GITLAB_IDEMPOTENCY_HEADER))
                    .ok_or(WebhookError::MissingHeader(GITLAB_EVENT_HEADER))?
            }
        };
        let id = id.trim();
        if id.is_empty() || id.len() > 256 {
            return Err(WebhookError::MalformedHeader {
                header: match self {
                    Self::GitHub(_) => GITHUB_DELIVERY_HEADER,
                    Self::GitLab(_) => GITLAB_EVENT_HEADER,
                },
                reason: "must contain 1 to 256 bytes".to_string(),
            });
        }
        Ok(VerifiedWebhook { id: id.to_string(), timestamp: chrono::Utc::now() })
    }
}

/// Compares two secrets in constant time: their SHA-256 digests have one length, whatever
/// the secrets' lengths, and every byte is compared.
fn same_secret(given: &[u8], expected: &[u8]) -> bool {
    let (given, expected) = (Sha256::digest(given), Sha256::digest(expected));
    given.iter().zip(expected.iter()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(headers: &'a [(&'static str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
        move |name| headers.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }

    #[test]
    fn github_signatures_verify_over_the_raw_body() {
        let forge = ForgeSignature::GitHub(ForgeSecret::new("It's a Secret to Everybody").unwrap());
        let good = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        let headers = [(GITHUB_SIGNATURE_HEADER, good), (GITHUB_DELIVERY_HEADER, "d-1")];
        assert_eq!(forge.verify(lookup(&headers), b"Hello, World!").unwrap().id, "d-1");
        assert_eq!(
            forge.verify(lookup(&headers), b"Hello, World?"),
            Err(WebhookError::SignatureMismatch)
        );
        let malformed = [(GITHUB_SIGNATURE_HEADER, "sha1=abc"), (GITHUB_DELIVERY_HEADER, "d-1")];
        assert!(matches!(
            forge.verify(lookup(&malformed), b"Hello, World!"),
            Err(WebhookError::MalformedHeader { .. })
        ));
        let unsigned = [(GITHUB_DELIVERY_HEADER, "d-1")];
        assert_eq!(
            forge.verify(lookup(&unsigned), b"Hello, World!"),
            Err(WebhookError::MissingHeader(GITHUB_SIGNATURE_HEADER))
        );
        let anonymous = [(GITHUB_SIGNATURE_HEADER, good)];
        assert_eq!(
            forge.verify(lookup(&anonymous), b"Hello, World!"),
            Err(WebhookError::MissingHeader(GITHUB_DELIVERY_HEADER))
        );
    }

    #[test]
    fn gitlab_tokens_match_exactly() {
        let forge = ForgeSignature::GitLab(ForgeSecret::new("token-1").unwrap());
        let headers = [(GITLAB_TOKEN_HEADER, "token-1"), (GITLAB_EVENT_HEADER, "e-1")];
        assert_eq!(forge.verify(lookup(&headers), b"{}").unwrap().id, "e-1");
        let retried = [(GITLAB_TOKEN_HEADER, "token-1"), (GITLAB_IDEMPOTENCY_HEADER, "k-1")];
        assert_eq!(forge.verify(lookup(&retried), b"{}").unwrap().id, "k-1");
        for wrong in ["token-2", "token-", "token-11", ""] {
            let headers = [(GITLAB_TOKEN_HEADER, wrong), (GITLAB_EVENT_HEADER, "e-1")];
            assert_eq!(
                forge.verify(lookup(&headers), b"{}"),
                Err(WebhookError::SignatureMismatch),
                "{wrong}"
            );
        }
    }
}
