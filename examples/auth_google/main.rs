//! Google Provider Example
//!
//! Demonstrates authentication with Google Identity.
//!
//! Run: cargo run --example auth_google --features sso

use adk_auth::sso::{ClaimsMapper, GoogleProvider, TokenClaims};
use adk_auth::{AccessControl, Permission, Role};

fn main() -> anyhow::Result<()> {
    println!("Google Provider Example");
    println!("=======================\n");

    // ==========================================================
    // Step 1: Create Google Provider
    // ==========================================================
    println!("1. Create GoogleProvider:");
    println!();

    // Your Google OAuth Client ID from Google Cloud Console
    let client_id = "your-client-id.apps.googleusercontent.com";
    let provider = GoogleProvider::new(client_id);

    println!("   let provider = GoogleProvider::new(\"{}\");", client_id);
    println!();
    println!("   Provider info:");
    println!("   - Issuer: https://accounts.google.com");
    println!("   - JWKS: https://www.googleapis.com/oauth2/v3/certs");
    println!("   - Client ID: {}", provider.client_id());
    println!();

    // ==========================================================
    // Step 2: Google Token Claims
    // ==========================================================
    println!("2. Google token claims:");
    println!();

    // Simulate what Google tokens contain
    let google_claims = TokenClaims {
        sub: "118234...".into(), // Google user ID
        iss: "https://accounts.google.com".into(),
        email: Some("alice@gmail.com".into()),
        email_verified: Some(true),
        name: Some("Alice Smith".into()),
        given_name: Some("Alice".into()),
        family_name: Some("Smith".into()),
        picture: Some("https://lh3.googleusercontent.com/a/...".into()),
        hd: Some("company.com".into()), // Google Workspace domain
        exp: 1735700000,
        iat: 1735696400,
        ..Default::default()
    };

    println!("   Standard claims:");
    println!("   - sub: {}", google_claims.sub);
    println!("   - email: {:?}", google_claims.email);
    println!("   - name: {:?}", google_claims.name);
    println!();
    println!("   Google-specific claims:");
    println!("   - hd (hosted domain): {:?}", google_claims.hd);
    println!("   - picture: {:?}", google_claims.picture);
    println!();

    // ==========================================================
    // Step 3: Map domain to roles
    // ==========================================================
    println!("3. Role mapping by domain:");
    println!();

    // You can use the 'hd' claim for Google Workspace users
    // to determine their organization
    let _mapper = ClaimsMapper::builder().user_id_from_email().default_role("external").build();

    // Typically you'd check hd (hosted domain) for org membership
    println!("   Google Workspace (hd claim):");
    println!("   - hd: \"company.com\" -> internal user");
    println!("   - hd: none -> external/personal Gmail");
    println!();

    // ==========================================================
    // Step 4: Build AccessControl
    // ==========================================================
    println!("4. AccessControl setup:");
    println!();

    let internal = Role::new("internal").allow(Permission::AllTools);

    let external = Role::new("external")
        .allow(Permission::Tool("search".into()))
        .deny(Permission::Tool("admin".into()));

    let _ac = AccessControl::builder().role(internal).role(external).build()?;

    println!("   Roles:");
    println!("   - internal: all tools");
    println!("   - external: search only, no admin");
    println!();

    // ==========================================================
    // Step 5: Domain-based access
    // ==========================================================
    println!("5. Domain-based access check:");
    println!();

    let test_users = [
        ("alice@company.com", Some("company.com"), "internal"),
        ("bob@gmail.com", None, "external"),
        ("charlie@partner.org", Some("partner.org"), "external"),
    ];

    for (email, hd, expected_role) in test_users {
        let role_name = match hd {
            Some("company.com") => "internal",
            _ => "external",
        };
        let emoji = if role_name == expected_role { "✅" } else { "❌" };
        println!("   {} {} (hd={:?}) -> {}", emoji, email, hd, role_name);
    }
    println!();

    // ==========================================================
    // Step 6: Full Integration
    // ==========================================================
    println!("6. Full integration example:");
    println!(
        r#"
   use adk_auth::sso::{{GoogleProvider, SsoAccessControl, ClaimsMapper}};

   // Setup
   let provider = GoogleProvider::new(std::env::var("GOOGLE_CLIENT_ID")?);
   
   let mapper = ClaimsMapper::builder()
       .user_id_from_email()
       .default_role("viewer")
       .build();

   let sso = SsoAccessControl::builder()
       .validator(provider)
       .mapper(mapper)
       .access_control(ac)
       .build()?;

   // Validate Google ID token
   let claims = sso.check_token(
       google_id_token,
       &Permission::Tool("search".into()),
   ).await?;

   // Check domain for internal users
   if claims.hd.as_deref() == Some("company.com") {{
       println!("Internal user: {{}}", claims.email.unwrap());
   }}
"#
    );

    Ok(())
}
