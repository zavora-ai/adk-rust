//! adk-auth SSO Example
//!
//! Demonstrates SSO integration with token validation and claims mapping.
//!
//! Run: cargo run --example auth_sso --features sso

#![allow(unused_imports)]

use adk_auth::sso::{
    AzureADProvider, ClaimsMapper, GoogleProvider, OktaProvider, SsoAccessControl, TokenClaims,
    TokenValidator,
};
use adk_auth::{AccessControl, Permission, Role};

fn main() -> anyhow::Result<()> {
    println!("adk-auth SSO Example");
    println!("====================\n");

    // ==========================================================
    // Step 1: Configure SSO Providers
    // ==========================================================
    println!("1. Available SSO Providers:");

    // Google
    let _google = GoogleProvider::new("your-google-client-id");
    println!("   - GoogleProvider: https://accounts.google.com");

    // Azure AD
    let _azure = AzureADProvider::new("your-tenant-id", "your-client-id");
    println!("   - AzureADProvider: https://login.microsoftonline.com/{{tenant}}/v2.0");

    // Okta
    let _okta = OktaProvider::new("your-domain.okta.com", "your-client-id");
    println!("   - OktaProvider: https://{{domain}}/oauth2/default");
    println!();

    // ==========================================================
    // Step 2: Define Roles
    // ==========================================================
    println!("2. Define adk-auth roles:");

    let admin = Role::new("admin").allow(Permission::AllTools).allow(Permission::AllAgents);

    let analyst = Role::new("analyst")
        .allow(Permission::Tool("search".into()))
        .allow(Permission::Tool("summarize".into()))
        .deny(Permission::Tool("code_exec".into()));

    let viewer = Role::new("viewer").allow(Permission::Tool("search".into()));

    println!("   - admin: all tools + agents");
    println!("   - analyst: search + summarize (no code_exec)");
    println!("   - viewer: search only");
    println!();

    // ==========================================================
    // Step 3: Configure Claims Mapper
    // ==========================================================
    println!("3. Claims mapping (IdP groups -> adk-auth roles):");

    let mapper = ClaimsMapper::builder()
        .map_group("AdminGroup", "admin")
        .map_group("Administrators", "admin")
        .map_group("DataAnalysts", "analyst")
        .map_group("Everyone", "viewer")
        .user_id_from_email()
        .default_role("viewer")
        .build();

    println!("   AdminGroup -> admin");
    println!("   DataAnalysts -> analyst");
    println!("   (default) -> viewer");
    println!();

    // ==========================================================
    // Step 4: Build AccessControl
    // ==========================================================
    let ac = AccessControl::builder().role(admin).role(analyst).role(viewer).build()?;

    println!("4. AccessControl: {} roles", ac.role_names().len());
    println!();

    // ==========================================================
    // Step 5: Simulate Claims Mapping
    // ==========================================================
    println!("5. Claims mapping simulation:");

    let test_claims = vec![
        create_claims("alice@company.com", vec!["AdminGroup"]),
        create_claims("bob@company.com", vec!["DataAnalysts"]),
        create_claims("charlie@company.com", vec![]),
    ];

    for claims in &test_claims {
        let user_id = mapper.get_user_id(claims);
        let roles = mapper.map_to_roles(claims);
        println!("   [user] -> {:?}", roles);
    }
    println!();

    // ==========================================================
    // Step 6: Check Permissions
    // ==========================================================
    println!("6. Permission checks:");

    let checks = [
        ("admin", "code_exec", true),
        ("analyst", "search", true),
        ("analyst", "code_exec", false),
        ("viewer", "search", true),
        ("viewer", "summarize", false),
    ];

    for (role, tool, expected) in checks {
        let role_obj = ac.get_role(role).unwrap();
        let perm = Permission::Tool(tool.into());
        let allowed = role_obj.can_access(&perm);
        let emoji = if allowed == expected { "✅" } else { "❌" };
        println!("   {} {} -> {} = {}", emoji, role, tool, allowed);
    }

    Ok(())
}

fn create_claims(email: &str, groups: Vec<&str>) -> TokenClaims {
    TokenClaims {
        sub: format!("user-{}", email.split('@').next().unwrap()),
        email: Some(email.to_string()),
        groups: groups.into_iter().map(String::from).collect(),
        ..Default::default()
    }
}
