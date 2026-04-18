//! Property tests for project-scoped memory isolation on search.
//!
//! These tests verify that the InMemoryMemoryService correctly isolates
//! project-scoped entries during search operations.

use adk_core::Content;
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
use chrono::Utc;
use proptest::prelude::*;

/// Fixed vocabulary for generating entry text. Using a small set ensures
/// reliable word-overlap matching during search.
const VOCAB: &[&str] = &["hello", "world", "test", "memory", "search", "project", "data", "entry"];

/// Generate a random text string by picking 1-4 words from the vocabulary.
fn arb_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(VOCAB), 1..=4)
        .prop_map(|words| words.join(" "))
}

/// Generate a simple project ID (alphanumeric, 1-20 chars).
fn arb_project_id() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{0,19}"
}

/// Generate a list of (text, scope) pairs where scope is:
/// - 0 = global
/// - 1 = project A
/// - 2 = project B
fn arb_entries(count: usize) -> impl Strategy<Value = Vec<(String, u8)>> {
    proptest::collection::vec((arb_text(), 0u8..3u8), 1..=count)
}

/// Helper to create a MemoryEntry from text.
fn make_entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "test-author".to_string(),
        timestamp: Utc::now(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: project-scoped-memory, Property 1: Project Isolation on Search**
    ///
    /// *For any* set of entries across projects A, B, and global scope,
    /// searching with `project_id = Some(A)` returns only global entries
    /// and entries from project A — never entries from project B.
    ///
    /// **Validates: Requirements 1.3, 3.1, 3.2, 6.2**
    #[test]
    fn prop_project_isolation_on_search(
        entries in arb_entries(20),
        project_a in arb_project_id(),
        project_b in arb_project_id(),
    ) {
        // Ensure projects are distinct
        prop_assume!(project_a != project_b);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let service = InMemoryMemoryService::new();
            let app = "test-app";
            let user = "test-user";

            // Track which texts belong to which scope
            let mut global_texts: Vec<String> = Vec::new();
            let mut project_a_texts: Vec<String> = Vec::new();
            let mut project_b_texts: Vec<String> = Vec::new();

            // Add entries to the service
            for (i, (text, scope)) in entries.iter().enumerate() {
                let session_id = format!("session-{i}");
                let entry = make_entry(text);
                match scope {
                    0 => {
                        service
                            .add_session(app, user, &session_id, vec![entry])
                            .await
                            .unwrap();
                        global_texts.push(text.to_lowercase());
                    }
                    1 => {
                        service
                            .add_session_to_project(
                                app, user, &session_id, &project_a, vec![entry],
                            )
                            .await
                            .unwrap();
                        project_a_texts.push(text.to_lowercase());
                    }
                    _ => {
                        service
                            .add_session_to_project(
                                app, user, &session_id, &project_b, vec![entry],
                            )
                            .await
                            .unwrap();
                        project_b_texts.push(text.to_lowercase());
                    }
                }
            }

            // Build a query that matches all vocabulary words
            let query = VOCAB.join(" ");

            // Search with project_id = Some(project_a)
            let resp = service
                .search(SearchRequest {
                    query,
                    user_id: user.to_string(),
                    app_name: app.to_string(),
                    limit: Some(1000),
                    min_score: None,
                    project_id: Some(project_a.clone()),
                })
                .await
                .unwrap();

            // Every result must be from global or project A, never project B only
            for mem in &resp.memories {
                let result_text = adk_memory::text::extract_text(&mem.content).to_lowercase();

                let is_global = global_texts.contains(&result_text);
                let is_project_a = project_a_texts.contains(&result_text);
                let is_project_b_only = project_b_texts.contains(&result_text)
                    && !is_global
                    && !is_project_a;

                assert!(
                    !is_project_b_only,
                    "Search with project_id=Some({project_a}) returned a project B-only entry: {result_text:?}",
                );
            }
        });
    }

    /// **Feature: project-scoped-memory, Property 2: Global-Only Search Excludes Project Entries**
    ///
    /// *For any* set of entries containing both global and project-scoped entries,
    /// searching with `project_id = None` returns only global entries.
    ///
    /// **Validates: Requirements 1.2, 3.3, 5.3**
    #[test]
    fn prop_global_only_search_excludes_project_entries(
        entries in arb_entries(20),
        project_id in arb_project_id(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let service = InMemoryMemoryService::new();
            let app = "test-app";
            let user = "test-user";

            let mut global_texts: Vec<String> = Vec::new();
            let mut project_texts: Vec<String> = Vec::new();

            // Add entries: scope 0 = global, scope 1 or 2 = project
            for (i, (text, scope)) in entries.iter().enumerate() {
                let session_id = format!("session-{i}");
                let entry = make_entry(text);
                if *scope == 0 {
                    service
                        .add_session(app, user, &session_id, vec![entry])
                        .await
                        .unwrap();
                    global_texts.push(text.to_lowercase());
                } else {
                    service
                        .add_session_to_project(
                            app, user, &session_id, &project_id, vec![entry],
                        )
                        .await
                        .unwrap();
                    project_texts.push(text.to_lowercase());
                }
            }

            // Search with project_id = None (global only)
            let query = VOCAB.join(" ");
            let resp = service
                .search(SearchRequest {
                    query,
                    user_id: user.to_string(),
                    app_name: app.to_string(),
                    limit: Some(1000),
                    min_score: None,
                    project_id: None,
                })
                .await
                .unwrap();

            // Every result must be a global entry
            for mem in &resp.memories {
                let result_text = adk_memory::text::extract_text(&mem.content).to_lowercase();

                let is_global = global_texts.contains(&result_text);
                let is_project_only = project_texts.contains(&result_text) && !is_global;

                assert!(
                    !is_project_only,
                    "Global-only search (project_id=None) returned a project-only entry: {result_text:?}",
                );
            }
        });
    }
}
