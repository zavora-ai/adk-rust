//! OIDC Discovery Example
//!
//! Demonstrates OpenID Connect provider with automatic discovery.
//!
//! Run: cargo run --example auth_oidc --features sso

use adk_auth::sso::{OidcProvider, TokenValidator};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("OIDC Discovery Example");
    println!("======================\n");

    // ==========================================================
    // OIDC Discovery URLs
    // ==========================================================
    println!("1. OIDC Discovery URLs:");
    println!();

    let providers = [
        ("Google", "https://accounts.google.com"),
        ("Microsoft", "https://login.microsoftonline.com/common/v2.0"),
        ("Okta", "https://dev-123456.okta.com/oauth2/default"),
        ("Auth0", "https://your-tenant.auth0.com"),
        ("Keycloak", "https://keycloak.example.com/realms/main"),
    ];

    for (name, issuer) in providers {
        println!("   {}: {}/.well-known/openid-configuration", name, issuer);
    }
    println!();

    // ==========================================================
    // Manual Configuration
    // ==========================================================
    println!("2. Manual OIDC configuration:");
    println!();

    // When you know the endpoints
    let _provider = OidcProvider::new(
        "https://accounts.google.com",
        "your-client-id",
        "https://www.googleapis.com/oauth2/v3/certs",
    );

    println!("   OidcProvider::new(");
    println!("       issuer,");
    println!("       client_id,");
    println!("       jwks_uri,");
    println!("   )");
    println!();

    // ==========================================================
    // Auto-Discovery (with real endpoint)
    // ==========================================================
    println!("3. OIDC auto-discovery:");
    println!();

    println!("   // Fetches .well-known/openid-configuration automatically");
    println!("   let provider = OidcProvider::from_discovery(");
    println!("       \"https://accounts.google.com\",");
    println!("       \"your-client-id\",");
    println!("   ).await?;");
    println!();

    // Try Google's real OIDC endpoint
    println!("   Attempting Google OIDC discovery...");
    match OidcProvider::from_discovery("https://accounts.google.com", "example-client-id").await {
        Ok(provider) => {
            println!("   ✅ Discovery successful!");
            println!("   Issuer: {}", provider.issuer());
        }
        Err(e) => {
            println!("   ⚠️  Discovery result: {}", e);
        }
    }
    println!();

    // ==========================================================
    // OIDC Configuration Response
    // ==========================================================
    println!("4. OIDC configuration response contains:");
    println!();
    println!("   {{");
    println!("     \"issuer\": \"https://accounts.google.com\",");
    println!("     \"authorization_endpoint\": \"https://accounts.google.com/o/oauth2/v2/auth\",");
    println!("     \"token_endpoint\": \"https://oauth2.googleapis.com/token\",");
    println!("     \"jwks_uri\": \"https://www.googleapis.com/oauth2/v3/certs\",");
    println!("     \"userinfo_endpoint\": \"https://openidconnect.googleapis.com/v1/userinfo\",");
    println!("     ...more fields...");
    println!("   }}");
    println!();

    // ==========================================================
    // Usage Pattern
    // ==========================================================
    println!("5. Usage pattern:");
    println!(
        r#"
   // At startup: discover and cache provider
   let provider = OidcProvider::from_discovery(
       "https://your-idp.com",
       std::env::var("CLIENT_ID")?,
   ).await?;

   // Per request: validate token
   let claims = provider.validate(bearer_token).await?;
   
   // Use claims
   let user_email = claims.email.unwrap_or(claims.sub);
   let is_admin = claims.groups.contains(&"admin".to_string());
"#
    );

    Ok(())
}
