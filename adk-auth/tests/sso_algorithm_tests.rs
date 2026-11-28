//! The JWKS validator accepts only the algorithms it has vetted.
//!
//! `JwtAlgorithm` is ADK-owned and lists RSA and EC algorithms only, so a symmetric
//! algorithm or `none` cannot reach a validator's allow-list, and a JWT library release
//! that adds an algorithm does not widen it.
#![cfg(feature = "sso")]

use adk_auth::sso::{JwtAlgorithm, JwtValidator, TokenError};

/// Symmetric algorithms would verify with a secret anyone can read from the JWKS, so they
/// do not parse, and neither does `none`.
#[test]
fn symmetric_algorithms_and_none_do_not_parse() {
    for name in ["HS256", "HS384", "HS512", "none", "EdDSA"] {
        let refused = name.parse::<JwtAlgorithm>();
        assert!(
            matches!(&refused, Err(TokenError::UnsupportedAlgorithm(_))),
            "{name}: {refused:?}"
        );
    }
}

/// The asymmetric set stays accepted.
#[test]
fn asymmetric_algorithms_remain_accepted() {
    let accepted = JwtValidator::builder()
        .issuer("https://issuer.example.com")
        .audience("https://api.example.com")
        .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
        .algorithm(JwtAlgorithm::RS256)
        .algorithm("ES256".parse().unwrap())
        .build()
        .is_ok();

    assert!(accepted, "RS256 and ES256 are the intended JWKS algorithms");
}
