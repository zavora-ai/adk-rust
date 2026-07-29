//! The JWKS validator must reject algorithms it has not vetted.
//!
//! `jsonwebtoken::Algorithm` is `#[non_exhaustive]`: a dependency release can add a variant
//! without a major bump on our side. An exhaustive match stopped compiling when v11 did exactly
//! that, and because no CI job built `--features sso`, nothing noticed.
#![cfg(feature = "sso")]

use adk_auth::sso::JwtValidator;

/// Symmetric algorithms cannot be validated against a public JWKS, so they are refused.
#[test]
fn hmac_algorithms_are_refused_for_jwks_validation() {
    // `JwtValidator` is not `Debug`, so match rather than `expect_err`.
    let Err(error) = JwtValidator::builder()
        .issuer("https://issuer.example.com")
        .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
        .algorithm(jsonwebtoken::Algorithm::HS256)
        .build()
    else {
        panic!("HS256 cannot be validated with a public key");
    };

    assert!(error.to_string().contains("not supported"), "{error}");
}

/// The asymmetric set stays accepted — the wildcard arm must not swallow valid algorithms.
#[test]
fn asymmetric_algorithms_remain_accepted() {
    let accepted = JwtValidator::builder()
        .issuer("https://issuer.example.com")
        .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
        .algorithm(jsonwebtoken::Algorithm::RS256)
        .algorithm(jsonwebtoken::Algorithm::ES256)
        .build()
        .is_ok();

    assert!(accepted, "RS256 and ES256 are the intended JWKS algorithms");
}
