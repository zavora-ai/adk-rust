//! Access control doc-test - validates access-control.md documentation

use adk_auth::{AccessControl, Permission, Role};
use adk_core::types::UserId;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Access Control Doc-Test ===\n");

    // From docs: Deny Precedence
    let role = Role::new("limited")
        .allow(Permission::AllTools)
        .deny(Permission::Tool("admin".to_string()));

    assert!(role.can_access(&Permission::Tool("search".to_string())));
    assert!(!role.can_access(&Permission::Tool("admin".to_string())));
    println!("✓ Deny precedence works");

    // From docs: Role creation
    let analyst = Role::new("analyst")
        .allow(Permission::Tool("search".to_string()))
        .allow(Permission::Tool("summarize".to_string()))
        .deny(Permission::Tool("code_exec".to_string()));

    assert!(analyst.can_access(&Permission::Tool("search".to_string())));
    assert!(!analyst.can_access(&Permission::Tool("code_exec".to_string())));
    println!("✓ Role creation works");

    // From docs: AccessControl builder
    let admin = Role::new("admin").allow(Permission::AllTools).allow(Permission::AllAgents);

    let ac = AccessControl::builder()
        .role(admin)
        .role(analyst)
        .assign(UserId::new("alice@company.com").unwrap(), "admin")
        .assign(UserId::new("bob@company.com").unwrap(), "analyst")
        .build()?;

    // From docs: Check permission
    let bob_id = UserId::new("bob@company.com").unwrap();
    ac.check(&bob_id, &Permission::Tool("search".to_string()))?;
    println!("✓ AccessControl builder and check works");

    // From docs: Multi-Role Union
    let reader = Role::new("reader").allow(Permission::Tool("search".to_string()));
    let writer = Role::new("writer").allow(Permission::Tool("write".to_string()));

    let ac2 = AccessControl::builder()
        .role(reader)
        .role(writer)
        .assign(UserId::new("alice").unwrap(), "reader")
        .assign(UserId::new("alice").unwrap(), "writer")
        .build()?;

    let alice_id = UserId::new("alice").unwrap();
    assert!(ac2.check(&alice_id, &Permission::Tool("search".to_string())).is_ok());
    assert!(ac2.check(&alice_id, &Permission::Tool("write".to_string())).is_ok());
    println!("✓ Multi-role union works");

    // From docs: Explicit Over Implicit
    let empty_role = Role::new("empty");
    assert!(!empty_role.can_access(&Permission::Tool("anything".to_string())));
    println!("✓ Explicit over implicit (empty role denies all)");

    println!("\n=== All access control tests passed! ===");
    Ok(())
}
