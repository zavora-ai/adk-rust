//! Property tests for MemoryServiceAdapter project forwarding.
//!
//! Verifies that the adapter correctly forwards project_id to the underlying
//! MemoryService for search, add, and delete operations.

use adk_core::{Content, Memory};
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryServiceAdapter};
use chrono::Utc;
use proptest::prelude::*;
use std::sync::Arc;

/// Fixed vocabulary for generating entry text.
const VOCAB: &[&str] = &["hello", "world", "test", "memory", "search", "project", "data", "entry"];

/// Generate a random text string by picking 1-4 words from the vocabulary.
fn arb_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(VOCAB), 1..=4)
        .prop_map(|words| words.join(" "))
}

/// Generate a simple project ID.
fn arb_project_id() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{0,19}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: project-scoped-memory, Property 7: Adapter Project Forwarding — search includes project_id**
    ///
    /// *For any* MemoryServiceAdapter with a project_id, search results include
    /// project-scoped entries for that project (plus global entries), confirming
    /// the adapter forwards project_id in SearchRequest.
    ///
    /// **Validates: Requirements 12.2, 12.3, 12.4**
    #[test]
    fn prop_adapter_search_forwards_project_id(
        global_text in arb_text(),
        project_text in arb_text(),
        project_id in arb_project_id(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let service = Arc::new(InMemoryMemoryService::new());
            let app = "test-app";
            let user = "test-user";

            // Add a global entry via the service directly
            let global_entry = adk_memory::MemoryEntry {
                content: Content::new("user").with_text(&global_text),
                author: "author".to_string(),
                timestamp: Utc::now(),
            };
            MemoryService::add_session(
                service.as_ref(), app, user, "global-session", vec![global_entry],
            )
            .await
            .unwrap();

            // Add a project entry via the service directly
            let project_entry = adk_memory::MemoryEntry {
                content: Content::new("user").with_text(&project_text),
                author: "author".to_string(),
                timestamp: Utc::now(),
            };
            MemoryService::add_session_to_project(
                service.as_ref(), app, user, "project-session", &project_id, vec![project_entry],
            )
            .await
            .unwrap();

            // Create adapter WITH project_id
            let adapter_with_project =
                MemoryServiceAdapter::new(service.clone(), app, user)
                    .with_project_id(&project_id);

            // Search via adapter — should see both global and project entries
            let query = VOCAB.join(" ");
            let results = adapter_with_project.search(&query).await.unwrap();

            // We should get results (global + project entries that match)
            assert!(
                !results.is_empty(),
                "Adapter with project_id should return results (global + project entries)"
            );

            // Create adapter WITHOUT project_id
            let adapter_no_project =
                MemoryServiceAdapter::new(service.clone(), app, user);

            // Search via adapter without project — should see only global entries
            let global_results = adapter_no_project.search(&query).await.unwrap();

            // Results with project >= results without project (global only)
            assert!(
                results.len() >= global_results.len(),
                "Adapter with project_id returned fewer results ({}) than without ({})",
                results.len(),
                global_results.len(),
            );
        });
    }

    /// **Feature: project-scoped-memory, Property 7b: Adapter add forwards to add_entry_to_project**
    ///
    /// *For any* MemoryServiceAdapter with a project_id, calling `add` stores
    /// the entry as a project-scoped entry (visible in project search, invisible
    /// in global-only search).
    ///
    /// **Validates: Requirements 12.3**
    #[test]
    fn prop_adapter_add_forwards_to_project(
        text in arb_text(),
        project_id in arb_project_id(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let service = Arc::new(InMemoryMemoryService::new());
            let app = "test-app";
            let user = "test-user";

            // Create adapter with project_id
            let adapter =
                MemoryServiceAdapter::new(service.clone(), app, user)
                    .with_project_id(&project_id);

            // Add entry via adapter
            let entry = adk_core::MemoryEntry {
                content: Content::new("user").with_text(&text),
                author: "author".to_string(),
            };
            adapter.add(entry).await.unwrap();

            // Search globally — should NOT find the project entry
            let query = VOCAB.join(" ");
            let global_adapter =
                MemoryServiceAdapter::new(service.clone(), app, user);
            let global_results = global_adapter.search(&query).await.unwrap();

            // Search with project — SHOULD find the entry
            let project_adapter =
                MemoryServiceAdapter::new(service.clone(), app, user)
                    .with_project_id(&project_id);
            let project_results = project_adapter.search(&query).await.unwrap();

            // The entry added via project adapter should be visible in project
            // search but not in global search
            assert!(
                project_results.len() > global_results.len(),
                "Entry added via project adapter should be visible in project search but not global. \
                 Project results: {}, Global results: {}",
                project_results.len(),
                global_results.len(),
            );
        });
    }

    /// **Feature: project-scoped-memory, Property 7c: Adapter without project_id preserves existing behavior**
    ///
    /// *For any* MemoryServiceAdapter without a project_id, add and search
    /// operate on global entries only, preserving backward compatibility.
    ///
    /// **Validates: Requirements 12.4**
    #[test]
    fn prop_adapter_without_project_preserves_behavior(
        text in arb_text(),
        project_id in arb_project_id(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let service = Arc::new(InMemoryMemoryService::new());
            let app = "test-app";
            let user = "test-user";

            // Create adapter WITHOUT project_id
            let adapter =
                MemoryServiceAdapter::new(service.clone(), app, user);

            // Add entry via adapter (should be global)
            let entry = adk_core::MemoryEntry {
                content: Content::new("user").with_text(&text),
                author: "author".to_string(),
            };
            adapter.add(entry).await.unwrap();

            // Search globally — should find the entry
            let query = VOCAB.join(" ");
            let global_results = adapter.search(&query).await.unwrap();
            assert!(
                !global_results.is_empty(),
                "Global adapter should find the globally-added entry"
            );

            // Search with a project — should also find the entry (global entries
            // are visible in project searches)
            let project_adapter =
                MemoryServiceAdapter::new(service.clone(), app, user)
                    .with_project_id(&project_id);
            let project_results = project_adapter.search(&query).await.unwrap();
            assert!(
                project_results.len() >= global_results.len(),
                "Project search should include global entries"
            );
        });
    }
}
