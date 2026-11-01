//! Tokens that carry a visitor id between a site and its visitors.

use std::sync::Arc;

use adk_core::subject::VisitorId;
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The name of the cookie a site may keep a visitor token in. The `__Host-` prefix makes a
/// browser accept it only when it is `Secure`, has no `Domain`, and has `Path=/`.
pub const VISITOR_COOKIE: &str = "__Host-adk_visitor";

/// Issues and verifies visitor tokens: `v1.{visitor id}.{expiry}.{HMAC-SHA256}`.
///
/// A token proves the bearer was given the visitor id; it authenticates no one. Tokens are
/// signed with the first key and verified with every key, so a deployment rotates by adding
/// a new first key and keeping the old one until its tokens expire. A site hands the token
/// to the visitor in a response header or, once the visitor consents to being remembered, in
/// the [`VISITOR_COOKIE`] cookie ([`set_cookie`](Self::set_cookie)); nothing sets a cookie by
/// default.
///
/// # Example
///
/// ```rust
/// use adk_auth::VisitorTokens;
/// use adk_core::subject::VisitorId;
///
/// let tokens = VisitorTokens::new(b"a key of at least thirty-two bytes!!".to_vec());
/// let visitor = VisitorId::generate();
/// let token = tokens.issue(&visitor);
/// assert_eq!(tokens.verify(&token), Some(visitor));
/// assert_eq!(tokens.verify(&token.replace("v1.", "v2.")), None);
///
/// let rotated = VisitorTokens::new(b"the next key, also thirty-two bytes".to_vec())
///     .with_previous_key(b"a key of at least thirty-two bytes!!".to_vec());
/// assert!(rotated.verify(&token).is_some(), "old tokens verify during rotation");
/// ```
#[derive(Clone)]
pub struct VisitorTokens {
    keys: Vec<Arc<[u8]>>,
    ttl: Duration,
}

impl std::fmt::Debug for VisitorTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisitorTokens")
            .field("keys", &self.keys.len())
            .field("ttl", &self.ttl)
            .finish()
    }
}

impl VisitorTokens {
    /// Tokens signed with `key`, at least 32 bytes kept by the site, lasting 30 days.
    pub fn new(key: impl Into<Arc<[u8]>>) -> Self {
        Self { keys: vec![key.into()], ttl: Duration::days(30) }
    }

    /// Also verifies tokens signed with `key`, an earlier key being rotated out.
    #[must_use]
    pub fn with_previous_key(mut self, key: impl Into<Arc<[u8]>>) -> Self {
        self.keys.push(key.into());
        self
    }

    /// Sets how long a token lasts.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// How long a token lasts.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    fn mac(key: &[u8], payload: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
        mac.update(payload.as_bytes());
        mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// A token for `visitor`, signed with the first key.
    pub fn issue(&self, visitor: &VisitorId) -> String {
        let expiry = (Utc::now() + self.ttl).timestamp();
        let payload = format!("v1.{visitor}.{expiry}");
        let mac = Self::mac(&self.keys[0], &payload);
        format!("{payload}.{mac}")
    }

    /// The visitor id `token` carries, when a key signed it and it has not expired.
    pub fn verify(&self, token: &str) -> Option<VisitorId> {
        let (payload, mac) = token.trim().rsplit_once('.')?;
        let signed = self.keys.iter().any(|key| {
            let expected = Self::mac(key, payload);
            expected.len() == mac.len()
                && expected.bytes().zip(mac.bytes()).fold(0_u8, |acc, (a, b)| acc | (a ^ b)) == 0
        });
        if !signed {
            return None;
        }
        let mut parts = payload.split('.');
        let (Some("v1"), Some(visitor), Some(expiry), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        if expiry.parse::<i64>().ok()? < Utc::now().timestamp() {
            return None;
        }
        visitor.parse().ok()
    }

    /// The `Set-Cookie` value that keeps `token` in the [`VISITOR_COOKIE`] cookie: `Secure`,
    /// `HttpOnly`, `SameSite=Lax`, `Path=/`, expiring with the token. Set it only once the
    /// visitor has consented to being remembered.
    pub fn set_cookie(&self, token: &str) -> String {
        format!(
            "{VISITOR_COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={}",
            self.ttl.num_seconds()
        )
    }

    /// The visitor token in a `Cookie` header value, if the [`VISITOR_COOKIE`] cookie is set.
    pub fn from_cookie_header(header: &str) -> Option<&str> {
        header.split(';').find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == VISITOR_COOKIE).then_some(value)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_verify_until_they_expire_and_never_across_keys() {
        let tokens = VisitorTokens::new(vec![7_u8; 32]);
        let visitor = VisitorId::generate();
        let token = tokens.issue(&visitor);
        assert_eq!(tokens.verify(&token), Some(visitor.clone()));
        assert_eq!(VisitorTokens::new(vec![8_u8; 32]).verify(&token), None);
        let expired = VisitorTokens::new(vec![7_u8; 32]).with_ttl(Duration::seconds(-60));
        assert_eq!(tokens.verify(&expired.issue(&visitor)), None);
        assert_eq!(tokens.verify("v1.subj_1.99999999999.00"), None);

        let cookie = tokens.set_cookie(&token);
        assert!(cookie.starts_with("__Host-adk_visitor=v1.v:"));
        assert!(cookie.contains("Secure; HttpOnly; SameSite=Lax"));
        let header = format!("theme=dark; {VISITOR_COOKIE}={token}; lang=sw");
        assert_eq!(VisitorTokens::from_cookie_header(&header), Some(token.as_str()));
    }
}
