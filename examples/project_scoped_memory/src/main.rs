//! Project-Scoped Memory — Complete Feature Demonstration
//!
//! This example walks through every capability of the project-scoped memory
//! feature using the `InMemoryMemoryService` backend. No external databases
//! required.
//!
//! Run with: `cargo run -p project-scoped-memory-example`
//!
//! ## Capabilities demonstrated
//!
//! 1. Global vs project-scoped entry storage
//! 2. Search isolation (global-only, project-scoped, cross-project)
//! 3. Project-scoped deletion without affecting other scopes
//! 4. Bulk project deletion via `delete_project`
//! 5. GDPR `delete_user` across all projects
//! 6. `MemoryServiceAdapter` with `with_project_id()` builder
//! 7. `adk_core::Memory` trait — `search_in_project` / `add_to_project`
//! 8. Project ID validation

use adk_core::{Content, Memory};
use adk_memory::{
    InMemoryMemoryService, MemoryEntry, MemoryService, MemoryServiceAdapter, SearchRequest,
    validate_project_id,
};
use chrono::Utc;
use std::sync::Arc;

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("assistant").with_text(text),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }
}

fn search_req(query: &str, project_id: Option<&str>) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        user_id: "user-1".to_string(),
        app_name: "demo-app".to_string(),
        limit: Some(100),
        min_score: None,
        project_id: project_id.map(String::from),
    }
}

fn print_results(label: &str, memories: &[impl std::fmt::Debug]) {
    println!("  {label}: {} result(s)", memories.len());
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = Arc::new(InMemoryMemoryService::new());
    let app = "demo-app";
    let user = "user-1";

    // ─────────────────────────────────────────────────────────────────────
    // 1. Store entries in different scopes
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 1. Storing entries across scopes ═══\n");

    // Global entries (no project)
    service
        .add_session(
            app,
            user,
            "global-s1",
            vec![
                entry("The Rust compiler catches memory bugs at compile time"),
                entry("Tokio provides an async runtime for Rust"),
            ],
        )
        .await?;
    println!("  ✓ Added 2 global entries");

    // Project A entries
    service
        .add_session_to_project(
            app,
            user,
            "proj-a-s1",
            "project-alpha",
            vec![
                entry("Project Alpha uses a microservices architecture"),
                entry("Alpha deploys to Kubernetes with Helm charts"),
            ],
        )
        .await?;
    println!("  ✓ Added 2 entries to project-alpha");

    // Project B entries
    service
        .add_session_to_project(
            app,
            user,
            "proj-b-s1",
            "project-beta",
            vec![
                entry("Project Beta is a monolith written in Rust"),
                entry("Beta uses SQLite for local storage"),
            ],
        )
        .await?;
    println!("  ✓ Added 2 entries to project-beta");

    // Single entry via add_entry_to_project
    service
        .add_entry_to_project(app, user, "project-alpha", entry("Alpha team meets every Monday"))
        .await?;
    println!("  ✓ Added 1 direct entry to project-alpha");

    println!();

    // ─────────────────────────────────────────────────────────────────────
    // 2. Search isolation
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 2. Search isolation ═══\n");

    // Global-only search (project_id = None)
    let global = service.search(search_req("Rust", None)).await?;
    print_results("Global search for 'Rust'", &global.memories);
    for m in &global.memories {
        println!("    → {}", adk_memory::text::extract_text(&m.content));
    }

    println!();

    // Project Alpha search (returns global + alpha entries)
    let alpha = service.search(search_req("Rust", Some("project-alpha"))).await?;
    print_results("Project-alpha search for 'Rust'", &alpha.memories);
    for m in &alpha.memories {
        println!("    → {}", adk_memory::text::extract_text(&m.content));
    }

    println!();

    // Project Beta search (returns global + beta entries)
    let beta = service.search(search_req("Rust", Some("project-beta"))).await?;
    print_results("Project-beta search for 'Rust'", &beta.memories);
    for m in &beta.memories {
        println!("    → {}", adk_memory::text::extract_text(&m.content));
    }

    println!();

    // Cross-project isolation: Alpha search should NOT see Beta entries
    let alpha_arch = service.search(search_req("monolith SQLite", Some("project-alpha"))).await?;
    print_results(
        "Project-alpha search for 'monolith SQLite' (Beta-only content)",
        &alpha_arch.memories,
    );
    assert!(alpha_arch.memories.is_empty(), "Alpha search must not return Beta-only entries");
    println!("  ✓ Cross-project isolation verified");

    println!();

    // ─────────────────────────────────────────────────────────────────────
    // 3. Project-scoped deletion
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 3. Project-scoped deletion ═══\n");

    // Delete entries matching "Kubernetes" in project-alpha only
    let deleted =
        service.delete_entries_in_project(app, user, "project-alpha", "Kubernetes Helm").await?;
    println!("  Deleted {deleted} entry(ies) matching 'Kubernetes Helm' in project-alpha");

    // Verify global entries are untouched
    let global_after = service.search(search_req("Rust", None)).await?;
    print_results("Global search after project-alpha delete", &global_after.memories);
    assert_eq!(global_after.memories.len(), global.memories.len());
    println!("  ✓ Global entries unaffected");

    // Verify project-beta is untouched
    let beta_after = service.search(search_req("Rust", Some("project-beta"))).await?;
    assert_eq!(beta_after.memories.len(), beta.memories.len());
    println!("  ✓ Project-beta entries unaffected");

    // Global delete_entries only removes global entries
    let global_deleted = service.delete_entries(app, user, "Tokio async runtime").await?;
    println!("  Deleted {global_deleted} global entry(ies) matching 'Tokio async runtime'");

    let global_remaining = service.search(search_req("Rust compiler", None)).await?;
    print_results("Global search after global delete", &global_remaining.memories);

    println!();

    // ─────────────────────────────────────────────────────────────────────
    // 4. Bulk project deletion
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 4. Bulk project deletion (delete_project) ═══\n");

    let beta_deleted = service.delete_project(app, user, "project-beta").await?;
    println!("  Deleted {beta_deleted} entry(ies) from project-beta");

    let beta_gone =
        service.search(search_req("monolith SQLite Beta", Some("project-beta"))).await?;
    // Should only see global entries now (no beta-specific ones)
    let global_check = service.search(search_req("monolith SQLite Beta", None)).await?;
    assert_eq!(
        beta_gone.memories.len(),
        global_check.memories.len(),
        "After delete_project, project search should only return global entries"
    );
    println!("  ✓ All project-beta entries removed");

    println!();

    // ─────────────────────────────────────────────────────────────────────
    // 5. MemoryServiceAdapter with project scope
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 5. MemoryServiceAdapter with project scope ═══\n");

    // Re-add some entries for the adapter demo
    service
        .add_session(
            app,
            user,
            "adapter-global",
            vec![entry("Global knowledge about Rust ownership")],
        )
        .await?;
    service
        .add_session_to_project(
            app,
            user,
            "adapter-proj",
            "project-gamma",
            vec![entry("Gamma project uses ownership patterns extensively")],
        )
        .await?;

    // Adapter WITHOUT project — sees only global
    let global_adapter = MemoryServiceAdapter::new(service.clone(), app, user);
    let global_results = global_adapter.search("ownership").await?;
    print_results("Adapter (no project) search 'ownership'", &global_results);

    // Adapter WITH project — sees global + project
    let project_adapter =
        MemoryServiceAdapter::new(service.clone(), app, user).with_project_id("project-gamma");
    let project_results = project_adapter.search("ownership").await?;
    print_results("Adapter (project-gamma) search 'ownership'", &project_results);
    assert!(
        project_results.len() >= global_results.len(),
        "Project adapter should return at least as many results as global"
    );
    println!("  ✓ Adapter project forwarding works");

    // Add via project adapter — entry goes to project scope
    project_adapter
        .add(adk_core::MemoryEntry {
            content: Content::new("user").with_text("Gamma needs better error handling"),
            author: "user".to_string(),
        })
        .await?;
    println!("  ✓ Added entry via project adapter (scoped to project-gamma)");

    // Verify it's invisible in global search
    let global_check = global_adapter.search("error handling").await?;
    let project_check = project_adapter.search("error handling").await?;
    println!(
        "  Global sees {} result(s), project-gamma sees {} result(s) for 'error handling'",
        global_check.len(),
        project_check.len()
    );
    assert!(project_check.len() > global_check.len());
    println!("  ✓ Project-scoped add via adapter verified");

    println!();

    // ─────────────────────────────────────────────────────────────────────
    // 6. Core Memory trait — search_in_project / add_to_project
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 6. Core Memory trait (search_in_project / add_to_project) ═══\n");

    let adapter = MemoryServiceAdapter::new(service.clone(), app, user);

    // add_to_project via the core trait
    adapter
        .add_to_project(
            adk_core::MemoryEntry {
                content: Content::new("user").with_text("Delta project uses graph databases"),
                author: "user".to_string(),
            },
            "project-delta",
        )
        .await?;
    println!("  ✓ Added entry to project-delta via Memory::add_to_project");

    // search_in_project via the core trait
    let delta_results = adapter.search_in_project("graph databases", "project-delta").await?;
    print_results("Memory::search_in_project('graph databases', 'project-delta')", &delta_results);
    assert!(!delta_results.is_empty());

    // Verify it's not in global search
    let global_graph = adapter.search("graph databases").await?;
    print_results("Memory::search('graph databases') — global only", &global_graph);
    assert!(global_graph.is_empty(), "Project-delta entry should not appear in global search");
    println!("  ✓ Core Memory trait project methods work correctly");

    println!();

    // ─────────────────────────────────────────────────────────────────────
    // 7. Project ID validation
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 7. Project ID validation ═══\n");

    // Empty project ID
    match validate_project_id("") {
        Err(e) => println!("  ✓ Empty project_id rejected: {e}"),
        Ok(()) => panic!("Empty project_id should be rejected"),
    }

    // Oversized project ID
    let long_id = "x".repeat(257);
    match validate_project_id(&long_id) {
        Err(e) => println!("  ✓ 257-char project_id rejected: {e}"),
        Ok(()) => panic!("257-char project_id should be rejected"),
    }

    // Valid project ID
    match validate_project_id("my-project-123") {
        Ok(()) => println!("  ✓ 'my-project-123' accepted"),
        Err(e) => panic!("Valid project_id rejected: {e}"),
    }

    // Boundary: exactly 256 chars
    let boundary_id = "a".repeat(256);
    match validate_project_id(&boundary_id) {
        Ok(()) => println!("  ✓ 256-char project_id accepted (boundary)"),
        Err(e) => panic!("256-char project_id rejected: {e}"),
    }

    // Validation is enforced on write operations
    let result = service.add_entry_to_project(app, user, "", entry("should fail")).await;
    match result {
        Err(e) => println!("  ✓ add_entry_to_project with empty project_id rejected: {e}"),
        Ok(()) => panic!("Empty project_id should be rejected on write"),
    }

    println!();

    // ─────────────────────────────────────────────────────────────────────
    // 8. GDPR delete_user — removes everything
    // ─────────────────────────────────────────────────────────────────────
    println!("═══ 8. GDPR delete_user ═══\n");

    // Count entries before
    let before_global = service.search(search_req("Rust ownership compiler", None)).await?;
    let before_alpha =
        service.search(search_req("Alpha Monday microservices", Some("project-alpha"))).await?;
    let before_gamma =
        service.search(search_req("ownership Gamma error", Some("project-gamma"))).await?;
    let before_delta =
        service.search(search_req("graph databases Delta", Some("project-delta"))).await?;
    println!(
        "  Before delete_user: global={}, alpha={}, gamma={}, delta={}",
        before_global.memories.len(),
        before_alpha.memories.len(),
        before_gamma.memories.len(),
        before_delta.memories.len()
    );

    service.delete_user(app, user).await?;
    println!("  ✓ delete_user called");

    // Verify everything is gone
    let after_global = service.search(search_req("Rust ownership compiler", None)).await?;
    let after_alpha =
        service.search(search_req("Alpha Monday microservices", Some("project-alpha"))).await?;
    let after_gamma =
        service.search(search_req("ownership Gamma error", Some("project-gamma"))).await?;
    let after_delta =
        service.search(search_req("graph databases Delta", Some("project-delta"))).await?;
    assert_eq!(after_global.memories.len(), 0);
    assert_eq!(after_alpha.memories.len(), 0);
    assert_eq!(after_gamma.memories.len(), 0);
    assert_eq!(after_delta.memories.len(), 0);
    println!(
        "  After delete_user:  global={}, alpha={}, gamma={}, delta={}",
        after_global.memories.len(),
        after_alpha.memories.len(),
        after_gamma.memories.len(),
        after_delta.memories.len()
    );
    println!("  ✓ All entries across all projects removed — GDPR compliant");

    println!("\n═══ All demonstrations complete ═══");
    Ok(())
}
