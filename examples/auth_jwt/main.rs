//! JWT Validation Example
//!
//! Demonstrates low-level JWT token validation with JWKS.
//!
//! Run: cargo run --example auth_jwt --features sso

#![allow(unused_imports)]

use adk_auth::sso::{JwtValidator, JwtValidatorBuilder, TokenClaims, TokenError};

fn main() -> anyhow::Result<()> {
    println!("JWT Validation Example");
    println!("======================\n");

    // ==========================================================
    // Building a JWT Validator
    // ==========================================================
    println!("1. Building a JWT Validator:");
    println!();

    // The validator requires:
    // - issuer: Expected token issuer (iss claim)
    // - jwks_uri: URL to fetch public keys
    // - audience: Optional expected audience (aud claim)

    println!("   JwtValidator::builder()");
    println!("       .issuer(\"https://example.auth0.com/\")");
    println!("       .jwks_uri(\"https://example.auth0.com/.well-known/jwks.json\")");
    println!("       .audience(\"api://my-app\")");
    println!("       .build()");
    println!();

    // Note: We can't actually build this without a real JWKS endpoint,
    // but here's how you would do it:
    let validator_result = JwtValidator::builder()
        .issuer("https://example.auth0.com/")
        .jwks_uri("https://example.auth0.com/.well-known/jwks.json")
        .audience("api://my-app")
        .build();

    match validator_result {
        Ok(_) => println!("   ✅ Validator built successfully"),
        Err(e) => println!("   ⚠️  Build result: {:?}", e),
    }
    println!();

    // ==========================================================
    // Token Claims Structure
    // ==========================================================
    println!("2. TokenClaims structure:");
    println!();

    let claims = TokenClaims {
        sub: "user-12345".into(),
        iss: "https://accounts.google.com".into(),
        email: Some("alice@example.com".into()),
        name: Some("Alice Smith".into()),
        groups: vec!["Engineering".into(), "Admins".into()],
        exp: 1735700000,
        iat: 1735696400,
        ..Default::default()
    };

    println!("   sub: [REDACTED]");
    println!("   iss: {}", claims.iss);
    println!("   email: [REDACTED]");
    println!("   name: [REDACTED]");
    println!("   groups: {:?}", claims.groups);
    println!("   exp: {} (Unix timestamp)", claims.exp);
    println!();

    // Helper methods
    println!("   Helper methods:");
    println!("   - user_id(): {}", claims.user_id());
    println!("   - all_groups(): {:?}", claims.all_groups());
    println!("   - is_expired(): {}", claims.is_expired());
    println!();

    // ==========================================================
    // Error Handling
    // ==========================================================
    println!("3. TokenError variants:");
    println!();

    let errors: Vec<TokenError> = vec![
        TokenError::Expired,
        TokenError::InvalidSignature,
        TokenError::InvalidIssuer {
            expected: "https://expected.com".into(),
            actual: "https://actual.com".into(),
        },
        TokenError::MissingClaim("email".into()),
        TokenError::KeyNotFound("key-123".into()),
    ];

    for err in errors {
        println!("   - {}", err);
    }
    println!();

    // ==========================================================
    // Usage Pattern
    // ==========================================================
    println!("4. Usage pattern:");
    println!();
    println!("   // Build validator once at startup");
    println!("   let validator = JwtValidator::builder()");
    println!("       .issuer(\"https://login.microsoftonline.com/tenant/v2.0\")");
    println!("       .jwks_uri(\"https://login.microsoftonline.com/tenant/discovery/v2.0/keys\")");
    println!("       .audience(\"api://my-app\")");
    println!("       .build()?;");
    println!();
    println!("   // Validate tokens from requests");
    println!("   async fn handle_request(token: &str) -> Result<(), TokenError> {{");
    println!("       let claims = validator.validate(token).await?;");
    println!("       println!(\"User: {{}}\", claims.email.unwrap_or(claims.sub));");
    println!("       Ok(())");
    println!("   }}");

    Ok(())
}
