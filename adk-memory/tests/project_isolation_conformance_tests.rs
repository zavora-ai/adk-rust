//! Project scoping must be real or refused, never silently global.
//!
//! `add_session_to_project`, `add_entry_to_project`, and `delete_entries_in_project`
//! had default implementations that discarded `project_id` and called their global
//! equivalents. `Memory::search_in_project` and `Memory::add_to_project` did the
//! same. A backend therefore compiled as project-aware without implementing a single
//! project method: calls carrying a project identifier succeeded while operating in
//! global scope, with nothing in the type system or the return value to say so. Data
//! meant for one project became visible to everything under the same app and user,
//! and a project-scoped delete could remove global entries.
//!
//! The defaults now fail, and `supports_project_scoping` lets a caller tell a
//! project-aware backend from one without support.

use adk_core::Content;
use adk_memory::inmemory::InMemoryMemoryService;
use adk_memory::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};
use async_trait::async_trait;

const APP: &str = "conformance-app";
const USER: &str = "conformance-user";

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: chrono::Utc::now(),
    }
}

/// The text of a stored entry, for readable assertions.
fn text_of(entry: &MemoryEntry) -> String {
    entry.content.parts.iter().filter_map(|part| part.text()).collect()
}

async fn search(service: &dyn MemoryService, query: &str, project: Option<&str>) -> Vec<String> {
    let response = service
        .search(SearchRequest {
            query: query.to_string(),
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            limit: None,
            min_score: None,
            project_id: project.map(str::to_string),
        })
        .await
        .expect("search must succeed");
    response.memories.iter().map(text_of).collect()
}

// ── A project-aware backend keeps its boundaries ───────────────────────

#[tokio::test]
async fn a_project_aware_backend_isolates_projects() {
    let service = InMemoryMemoryService::new();
    assert!(
        service.supports_project_scoping(),
        "a backend that implements the project methods must advertise support"
    );

    service.add_entry(APP, USER, entry("shared knowledge")).await.unwrap();
    service
        .add_entry_to_project(APP, USER, "project-a", entry("knowledge for alpha"))
        .await
        .unwrap();
    service
        .add_entry_to_project(APP, USER, "project-b", entry("knowledge for beta"))
        .await
        .unwrap();

    let in_a = search(&service, "knowledge", Some("project-a")).await;
    assert!(
        in_a.iter().any(|c| c.contains("alpha")),
        "a project search must see its own entries: {in_a:?}"
    );
    assert!(
        !in_a.iter().any(|c| c.contains("beta")),
        "a project search must not see another project's entries: {in_a:?}"
    );

    let global = search(&service, "knowledge", None).await;
    assert!(
        !global.iter().any(|c| c.contains("alpha") || c.contains("beta")),
        "a global search must not see project entries: {global:?}"
    );
}

#[tokio::test]
async fn a_project_scoped_delete_does_not_reach_other_scopes() {
    let service = InMemoryMemoryService::new();

    service.add_entry(APP, USER, entry("deletable global")).await.unwrap();
    service.add_entry_to_project(APP, USER, "project-a", entry("deletable alpha")).await.unwrap();
    service.add_entry_to_project(APP, USER, "project-b", entry("deletable beta")).await.unwrap();

    service.delete_entries_in_project(APP, USER, "project-a", "deletable").await.unwrap();

    assert!(
        search(&service, "deletable", Some("project-b")).await.iter().any(|c| c.contains("beta")),
        "another project's entries must survive a project-scoped delete"
    );
    assert!(
        search(&service, "deletable", None).await.iter().any(|c| c.contains("global")),
        "global entries must survive a project-scoped delete"
    );
}

// ── A backend without project support refuses rather than widening scope ─

/// A backend that implements only the global surface, as a third-party backend or
/// `GraphMemoryService` does.
///
/// It implements `delete_entries`, so a project-scoped delete that fell back to the
/// global one would be observable here rather than merely erroring for want of an
/// implementation.
#[derive(Default)]
struct GlobalOnlyBackend {
    global_deletes: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl MemoryService for GlobalOnlyBackend {
    async fn add_session(
        &self,
        _app_name: &str,
        _user_id: &str,
        _session_id: &str,
        _entries: Vec<MemoryEntry>,
    ) -> adk_core::Result<()> {
        Ok(())
    }

    async fn search(&self, _req: SearchRequest) -> adk_core::Result<SearchResponse> {
        Ok(SearchResponse { memories: Vec::new() })
    }

    async fn delete_entries(
        &self,
        _app_name: &str,
        _user_id: &str,
        _query: &str,
    ) -> adk_core::Result<u64> {
        self.global_deletes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(7)
    }
}

#[tokio::test]
async fn a_backend_without_project_support_reports_it() {
    assert!(
        !GlobalOnlyBackend::default().supports_project_scoping(),
        "a backend that implements no project method must not advertise support"
    );
}

#[tokio::test]
async fn project_writes_fail_rather_than_silently_going_global() {
    let backend = GlobalOnlyBackend::default();

    let session_write = backend
        .add_session_to_project(APP, USER, "session-1", "project-a", vec![entry("secret")])
        .await;
    assert!(
        session_write.is_err(),
        "a project session write must fail instead of writing globally"
    );
    let message = session_write.unwrap_err().to_string();
    assert!(
        message.contains("project scoping"),
        "the error must say why the write was refused: {message}"
    );

    assert!(
        backend.add_entry_to_project(APP, USER, "project-a", entry("secret")).await.is_err(),
        "a project entry write must fail instead of writing globally"
    );
}

#[tokio::test]
async fn a_project_scoped_delete_fails_rather_than_deleting_globally() {
    // The most damaging fallback: a scoped delete widening to every entry.
    let backend = GlobalOnlyBackend::default();
    let result = backend.delete_entries_in_project(APP, USER, "project-a", "anything").await;

    assert!(result.is_err(), "a project-scoped delete must fail instead of deleting globally");
    assert_eq!(
        backend.global_deletes.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the scoped delete reached the global delete, so it removed entries outside the project"
    );
}
