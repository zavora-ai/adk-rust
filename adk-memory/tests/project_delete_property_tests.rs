//! Property tests for project-scoped memory deletion isolation and GDPR compliance.
//!
//! These tests verify that delete operations in one scope do not affect entries
//! in other scopes, and that delete_user removes all entries across all projects.

use adk_core::Content;
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
use chrono::Utc;
use proptest::prelude::*;

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

/// Helper to count search results for a given scope.
async fn count_results(
    service: &InMemoryMemoryService,
    app: &str,
    user: &str,
    project_id: Option<String>,
) -> usize {
    let query = VOCAB.join(" ");
    let resp = service
        .search(SearchRequest {
            query,
            user_id: user.to_string(),
            app_name: app.to_string(),
            limit: Some(10000),
            min_score: None,
            project_id,
        })
        .await
        .unwrap();
    resp.memories.len()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: project-scoped-memory, Property 3: Delete Isolation**
    ///
    /// *For any* set of entries distributed across projects A, B, and global scope,
    /// deleting entries in project A does not affect entries in project B or global
    /// entries. Deleting global entries does not affect project-scoped entries.
    ///
    /// **Validates: Requirements 4.2, 4.3, 6.3**
    #[test]
    fn prop_delete_isolation(
        entries in arb_entries(15),
        project_a in arb_project_id(),
        project_b in arb_project_id(),
    ) {
        prop_assume!(project_a != project_b);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let service = InMemoryMemoryService::new();
            let app = "test-app";
            let user = "test-user";

            let mut project_a_count: usize = 0;
            let mut project_b_count: usize = 0;

            // Add entries
            for (i, (text, scope)) in entries.iter().enumerate() {
                let session_id = format!("session-{i}");
                let entry = make_entry(text);
                match scope {
                    0 => {
                        service
                            .add_session(app, user, &session_id, vec![entry])
                            .await
                            .unwrap();
                    }
                    1 => {
                        service
                            .add_session_to_project(
                                app, user, &session_id, &project_a, vec![entry],
                            )
                            .await
                            .unwrap();
                        project_a_count += 1;
                    }
                    _ => {
                        service
                            .add_session_to_project(
                                app, user, &session_id, &project_b, vec![entry],
                            )
                            .await
                            .unwrap();
                        project_b_count += 1;
                    }
                }
            }

            // Count results before deletion
            let global_before = count_results(&service, app, user, None).await;
            let b_before =
                count_results(&service, app, user, Some(project_b.clone())).await;

            // Delete all entries in project A
            if project_a_count > 0 {
                service.delete_project(app, user, &project_a).await.unwrap();
            }

            // Verify global entries are unaffected
            let global_after = count_results(&service, app, user, None).await;
            assert_eq!(
                global_before, global_after,
                "Deleting project A entries affected global entries"
            );

            // Verify project B entries are unaffected
            let b_after =
                count_results(&service, app, user, Some(project_b.clone())).await;
            assert_eq!(
                b_before, b_after,
                "Deleting project A entries affected project B entries"
            );

            // Verify project A entries are gone — searching project A should
            // return only global entries now
            let a_after =
                count_results(&service, app, user, Some(project_a.clone())).await;
            assert_eq!(
                a_after, global_after,
                "Project A should have no project-specific entries after delete_project"
            );

            // Now test: deleting global entries doesn't affect project B
            let query = VOCAB.join(" ");
            service.delete_entries(app, user, &query).await.unwrap();

            // Project B entries should still be there
            let b_after_global_delete =
                count_results(&service, app, user, Some(project_b.clone())).await;
            // After deleting global entries, project B search returns only project B entries
            assert_eq!(
                b_after_global_delete, project_b_count,
                "Deleting global entries affected project B entries (expected {project_b_count}, got {b_after_global_delete})",
            );
        });
    }

    /// **Feature: project-scoped-memory, Property 5: GDPR delete_user Completeness**
    ///
    /// *For any* user with entries across N projects plus global entries,
    /// calling `delete_user` removes all entries for that user.
    ///
    /// **Validates: Requirements 15.1, 15.2**
    #[test]
    fn prop_gdpr_delete_user_completeness(
        entries in arb_entries(15),
        project_a in arb_project_id(),
        project_b in arb_project_id(),
    ) {
        prop_assume!(project_a != project_b);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let service = InMemoryMemoryService::new();
            let app = "test-app";
            let user = "test-user";

            // Add entries across global, project A, and project B
            for (i, (text, scope)) in entries.iter().enumerate() {
                let session_id = format!("session-{i}");
                let entry = make_entry(text);
                match scope {
                    0 => {
                        service
                            .add_session(app, user, &session_id, vec![entry])
                            .await
                            .unwrap();
                    }
                    1 => {
                        service
                            .add_session_to_project(
                                app, user, &session_id, &project_a, vec![entry],
                            )
                            .await
                            .unwrap();
                    }
                    _ => {
                        service
                            .add_session_to_project(
                                app, user, &session_id, &project_b, vec![entry],
                            )
                            .await
                            .unwrap();
                    }
                }
            }

            // Delete user
            service.delete_user(app, user).await.unwrap();

            // Verify all entries are gone
            let global_results = count_results(&service, app, user, None).await;
            assert_eq!(global_results, 0, "delete_user left {global_results} global entries");

            let a_results =
                count_results(&service, app, user, Some(project_a.clone())).await;
            assert_eq!(a_results, 0, "delete_user left {a_results} entries in project A");

            let b_results =
                count_results(&service, app, user, Some(project_b.clone())).await;
            assert_eq!(b_results, 0, "delete_user left {b_results} entries in project B");
        });
    }
}
