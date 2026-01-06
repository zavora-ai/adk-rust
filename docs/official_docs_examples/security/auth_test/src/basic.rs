//! Access control doc-test - validates access-control.md documentation

use adk_auth::{AccessControl, Permission, Role};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Access Control Doc-Test ===\n");

    // From docs: Deny Precedence
    let role = Role::new("limited")
        .allow(Permission::AllTools)
        .deny(Permission::Tool("admin".into()));

    assert!(role.can_access(&Permission::Tool("search".into())));
    assert!(!role.can_access(&Permission::Tool("admin".into())));
    println!("✓ Deny precedence works");

    // From docs: Role creation
    let analyst = Role::new("analyst")
        .allow(Permission::Tool("search".into()))
        .allow(Permission::Tool("summarize".into()))
        .deny(Permission::Tool("code_exec".into()));

    assert!(analyst.can_access(&Permission::Tool("search".into())));
    assert!(!analyst.can_access(&Permission::Tool("code_exec".into())));
    println!("✓ Role creation works");

    // From docs: AccessControl builder
    let admin = Role::new("admin")
        .allow(Permission::AllTools)
        .allow(Permission::AllAgents);

    let ac = AccessControl::builder()
        .role(admin)
        .role(analyst)
        .assign("alice@company.com", "admin")
        .assign("bob@company.com", "analyst")
        .build()?;

    // From docs: Check permission
    ac.check("bob@company.com", &Permission::Tool("search".into()))?;
    println!("✓ AccessControl builder and check works");

    // From docs: Multi-Role Union
    let reader = Role::new("reader").allow(Permission::Tool("search".into()));
    let writer = Role::new("writer").allow(Permission::Tool("write".into()));

    let ac2 = AccessControl::builder()
        .role(reader)
        .role(writer)
        .assign("alice", "reader")
        .assign("alice", "writer")
        .build()?;

    assert!(ac2.check("alice", &Permission::Tool("search".into())).is_ok());
    assert!(ac2.check("alice", &Permission::Tool("write".into())).is_ok());
    println!("✓ Multi-role union works");

    // From docs: Explicit Over Implicit
    let empty_role = Role::new("empty");
    assert!(!empty_role.can_access(&Permission::Tool("anything".into())));
    println!("✓ Explicit over implicit (empty role denies all)");

    println!("\n=== All access control tests passed! ===");
    Ok(())
}
