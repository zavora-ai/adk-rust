//! Trust tiers from verified credentials only (spec 3.5 §7.3, A-I6).
//!
//! A [`TierAssigner`] reads a request's head and returns the caller's tier and, above
//! `public`, its verified requester id. The default, [`PublicTierAssigner`], is fail-closed:
//! every caller is `public`. Web Bot Auth, OAuth, mTLS and API-key assigners plug in here.
//! Self-declared headers become a [`claimed_agent`] label for analytics and never change a
//! tier.

use async_trait::async_trait;
use awp_types::{RequesterId, RequesterType, TrustTier};
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::detect::detect_requester_type;

/// A caller's tier and, from `known` upwards, its verified requester id (spec 3.5 §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierAssignment {
    /// The caller's tier.
    pub tier: TrustTier,
    /// The verified requester id; `None` for `public`.
    pub requester: Option<RequesterId>,
}

impl TierAssignment {
    /// The `public` assignment, with no requester id.
    pub fn public() -> Self {
        Self { tier: TrustTier::Public, requester: None }
    }

    /// An assignment above `public` for a verified requester.
    pub fn verified(tier: TrustTier, requester: RequesterId) -> Self {
        Self { tier, requester: Some(requester) }
    }
}

/// Assigns a request's tier from its verified credentials (spec 3.5 §7.3, A-I6).
///
/// Implementations verify a credential — a Web Bot Auth signature, an OAuth token, a client
/// certificate, an owner-issued API key — and apply the owner's trust rules. A claim, such as
/// a `User-Agent` or `X-AWP-Channel` header, must never raise the tier.
///
/// # Example
///
/// ```
/// use adk_awp::v2::tier::{PublicTierAssigner, TierAssigner};
/// use awp_types::TrustTier;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let (head, _) = axum::http::Request::new(()).into_parts();
/// assert_eq!(PublicTierAssigner.assign(&head).await.tier, TrustTier::Public);
/// # }
/// ```
#[async_trait]
pub trait TierAssigner: Send + Sync {
    /// Returns the tier of the request whose head is `head`.
    async fn assign(&self, head: &Parts) -> TierAssignment;
}

/// The fail-closed default: every caller is `public` (spec 3.5 A-I6).
#[derive(Debug, Clone, Copy, Default)]
pub struct PublicTierAssigner;

#[async_trait]
impl TierAssigner for PublicTierAssigner {
    async fn assign(&self, _head: &Parts) -> TierAssignment {
        TierAssignment::public()
    }
}

/// The agent a request claims to be, for analytics only (spec 3.5 §7.3).
///
/// Returns the `User-Agent` product token, or `X-AWP-Channel`, of a request the 1.x heuristic
/// classifies as an agent. The label never affects a tier, a limit or what a caller sees.
///
/// # Example
///
/// ```
/// use adk_awp::v2::tier::claimed_agent;
/// use axum::http::HeaderMap;
///
/// let mut headers = HeaderMap::new();
/// headers.insert("user-agent", "GPTBot/1.2 (+https://openai.com/gptbot)".parse().unwrap());
/// headers.insert("accept", "application/json".parse().unwrap());
/// assert_eq!(claimed_agent(&headers).as_deref(), Some("GPTBot/1.2"));
/// ```
pub fn claimed_agent(headers: &HeaderMap) -> Option<String> {
    if detect_requester_type(headers) != RequesterType::Agent {
        return None;
    }
    let product = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .and_then(|agent| agent.split_whitespace().next())
        .filter(|token| !token.is_empty())
        .map(|token| token.chars().take(64).collect());
    product.or_else(|| Some("x-awp-channel".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claims never raise trust (spec 3.5 S6, A-I6).
    #[tokio::test]
    async fn claims_never_raise_the_default_tier() {
        let request = axum::http::Request::builder()
            .header("user-agent", "GPTBot/1.2")
            .header("x-awp-channel", "agent")
            .header("authorization", "Bearer forged")
            .body(())
            .unwrap();
        let (head, ()) = request.into_parts();
        assert_eq!(PublicTierAssigner.assign(&head).await, TierAssignment::public());
        assert_eq!(claimed_agent(&head.headers).as_deref(), Some("GPTBot/1.2"));
    }

    #[test]
    fn a_human_browser_claims_no_agent() {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "Mozilla/5.0".parse().unwrap());
        headers.insert("accept", "text/html".parse().unwrap());
        assert_eq!(claimed_agent(&headers), None);
    }
}
