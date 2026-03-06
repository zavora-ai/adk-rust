//! Integration tests for adk-auth SSO functionality.

#![cfg(feature = "sso")]

use adk_auth::sso::{
    ClaimsMapper, GoogleProvider, OidcProvider, SsoAccessControl, TokenClaims, TokenValidator,
};
use adk_auth::{AccessControl, Permission, Role};

// =============================================================================
// TokenClaims Tests
// =============================================================================

#[test]
fn test_token_claims_defaults() {
    let claims = TokenClaims::default();
    assert!(claims.sub.is_empty());
    assert!(claims.email.is_none());
    assert!(claims.groups.is_empty());
}

#[test]
fn test_token_claims_user_id() {
    let claims = TokenClaims {
        sub: "user-123".to_string(),
        email: Some("alice@example.com".to_string()),
        ..Default::default()
    };

    // Prefers email over sub
    assert_eq!(&*claims.user_id(), "alice@example.com");

    let claims_no_email =
        TokenClaims { sub: "user-123".to_string(), email: None, ..Default::default() };
    assert_eq!(&*claims_no_email.user_id(), "user-123");
}

#[test]
fn test_token_claims_all_groups() {
    let claims = TokenClaims {
        groups: vec!["group1".to_string(), "group2".to_string()],
        roles: vec!["role1".to_string()],
        ..Default::default()
    };

    let all = claims.all_groups();
    assert_eq!(all.len(), 3);
    assert!(all.contains(&"group1"));
    assert!(all.contains(&"group2"));
    assert!(all.contains(&"role1"));
}

#[test]
fn test_token_claims_expiry() {
    let expired = TokenClaims {
        exp: 0, // Epoch time - definitely expired
        ..Default::default()
    };
    assert!(expired.is_expired());

    let future = TokenClaims {
        exp: u64::MAX, // Far future
        ..Default::default()
    };
    assert!(!future.is_expired());
}

// =============================================================================
// ClaimsMapper Tests
// =============================================================================

#[test]
fn test_claims_mapper_group_to_role() {
    let mapper =
        ClaimsMapper::builder().map_group("AdminGroup", "admin").map_group("Users", "user").build();

    let claims = TokenClaims { groups: vec!["AdminGroup".to_string()], ..Default::default() };

    let roles = mapper.map_to_roles(&claims);
    assert_eq!(roles, vec!["admin"]);
}

#[test]
fn test_claims_mapper_multiple_groups() {
    let mapper =
        ClaimsMapper::builder().map_group("Group1", "role1").map_group("Group2", "role2").build();

    let claims = TokenClaims {
        groups: vec!["Group1".to_string(), "Group2".to_string()],
        ..Default::default()
    };

    let roles = mapper.map_to_roles(&claims);
    assert!(roles.contains(&"role1".to_string()));
    assert!(roles.contains(&"role2".to_string()));
}

#[test]
fn test_claims_mapper_default_role() {
    let mapper = ClaimsMapper::builder().map_group("Admin", "admin").default_role("guest").build();

    // No matching groups - should get default
    let claims = TokenClaims { groups: vec!["Unknown".to_string()], ..Default::default() };

    let roles = mapper.map_to_roles(&claims);
    assert_eq!(roles, vec!["guest"]);
}

#[test]
fn test_claims_mapper_user_id_from_email() {
    let mapper = ClaimsMapper::builder().user_id_from_email().build();

    let claims = TokenClaims {
        sub: "user-123".to_string(),
        email: Some("alice@example.com".to_string()),
        ..Default::default()
    };

    assert_eq!(&*mapper.get_user_id(&claims), "alice@example.com");
}

#[test]
fn test_claims_mapper_user_id_from_sub() {
    let mapper = ClaimsMapper::builder().user_id_from_sub().build();

    let claims = TokenClaims {
        sub: "user-123".to_string(),
        email: Some("alice@example.com".to_string()),
        ..Default::default()
    };

    assert_eq!(&*mapper.get_user_id(&claims), "user-123");
}

#[test]
fn test_claims_mapper_user_id_from_preferred_username() {
    let mapper = ClaimsMapper::builder().user_id_from_preferred_username().build();

    let claims = TokenClaims {
        sub: "user-123".to_string(),
        preferred_username: Some("alice".to_string()),
        ..Default::default()
    };

    assert_eq!(&*mapper.get_user_id(&claims), "alice");
}

// =============================================================================
// Provider Construction Tests
// =============================================================================

#[test]
fn test_google_provider_construction() {
    let provider = GoogleProvider::new("test-client-id");
    assert_eq!(provider.client_id(), "test-client-id");
}

#[test]
fn test_oidc_provider_manual_construction() {
    let provider = OidcProvider::new(
        "https://accounts.google.com",
        "test-client-id",
        "https://www.googleapis.com/oauth2/v3/certs",
    );
    assert_eq!(provider.client_id(), "test-client-id");
}

// =============================================================================
// SsoAccessControl Tests
// =============================================================================

#[test]
fn test_sso_access_control_builder() {
    let role = Role::new("user").allow(Permission::Tool("search".into()));

    let ac = AccessControl::builder().role(role).build().unwrap();

    let mapper = ClaimsMapper::builder().map_group("Users", "user").default_role("guest").build();

    let provider = GoogleProvider::new("test-client-id");

    let result =
        SsoAccessControl::builder().validator(provider).mapper(mapper).access_control(ac).build();

    assert!(result.is_ok());
}

#[test]
fn test_sso_access_control_missing_validator() {
    let ac = AccessControl::builder().role(Role::new("user")).build().unwrap();

    let result = SsoAccessControl::builder().access_control(ac).build();

    assert!(result.is_err());
}

#[test]
fn test_sso_access_control_missing_access_control() {
    let provider = GoogleProvider::new("test-client-id");

    let result = SsoAccessControl::builder().validator(provider).build();

    assert!(result.is_err());
}

// =============================================================================
// OIDC Discovery Tests (requires network)
// =============================================================================

#[tokio::test]
async fn test_google_oidc_discovery() {
    // This test requires network access to Google's OIDC endpoint
    let result =
        OidcProvider::from_discovery("https://accounts.google.com", "test-client-id").await;

    match result {
        Ok(provider) => {
            assert_eq!(provider.issuer(), "https://accounts.google.com");
        }
        Err(e) => {
            // Network issues are acceptable in CI
            eprintln!("OIDC discovery test skipped (network): {}", e);
        }
    }
}

// =============================================================================
// Integration Flow Tests
// =============================================================================

#[test]
fn test_complete_sso_flow_setup() {
    // Step 1: Define roles
    let admin = Role::new("admin").allow(Permission::AllTools);
    let analyst = Role::new("analyst")
        .allow(Permission::Tool("search".into()))
        .deny(Permission::Tool("admin".into()));
    let viewer = Role::new("viewer").allow(Permission::Tool("view".into()));

    // Step 2: Build AccessControl
    let ac = AccessControl::builder().role(admin).role(analyst).role(viewer).build().unwrap();

    // Step 3: Configure ClaimsMapper
    let mapper = ClaimsMapper::builder()
        .map_group("AdminGroup", "admin")
        .map_group("Analysts", "analyst")
        .map_group("Everyone", "viewer")
        .default_role("viewer")
        .user_id_from_email()
        .build();

    // Step 4: Build SsoAccessControl
    let provider = GoogleProvider::new("client-id");
    let sso = SsoAccessControl::builder()
        .validator(provider)
        .mapper(mapper)
        .access_control(ac)
        .build()
        .unwrap();

    // Verify it built correctly
    assert!(sso.access_control().role_names().contains(&"admin"));
    assert!(sso.access_control().role_names().contains(&"analyst"));
    assert!(sso.access_control().role_names().contains(&"viewer"));
}
