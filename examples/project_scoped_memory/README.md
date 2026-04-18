# Project-Scoped Memory Example

Demonstrates all capabilities of the project-scoped memory feature in `adk-memory`.

## What it shows

1. **Global vs project-scoped storage** — entries stored with `add_session` (global) vs `add_session_to_project` / `add_entry_to_project` (project-scoped)
2. **Search isolation** — global search returns only global entries; project search returns global + that project's entries; entries from other projects are never visible
3. **Project-scoped deletion** — `delete_entries_in_project` removes only matching entries within a project, leaving global and other projects untouched
4. **Bulk project deletion** — `delete_project` removes all entries for a project in one call
5. **GDPR delete_user** — removes all entries across all projects and global scope
6. **MemoryServiceAdapter** — `with_project_id()` builder scopes all adapter operations to a project
7. **Core Memory trait** — `search_in_project()` and `add_to_project()` on `adk_core::Memory`
8. **Project ID validation** — `validate_project_id()` rejects empty and oversized identifiers

## Run

```bash
cargo run -p project-scoped-memory-example
```

No external databases or API keys required — uses the in-memory backend.

## Key APIs

```rust
// Store entries in a project
service.add_session_to_project(app, user, session, "my-project", entries).await?;
service.add_entry_to_project(app, user, "my-project", entry).await?;

// Search with project scope
let results = service.search(SearchRequest {
    query: "my query".into(),
    project_id: Some("my-project".into()),  // None = global only
    ..
}).await?;

// Delete within a project
service.delete_entries_in_project(app, user, "my-project", "query").await?;
service.delete_project(app, user, "my-project").await?;

// Adapter with project binding
let adapter = MemoryServiceAdapter::new(service, app, user)
    .with_project_id("my-project");

// Core trait methods
adapter.search_in_project("query", "my-project").await?;
adapter.add_to_project(entry, "my-project").await?;

// Validation
validate_project_id("my-project")?;  // Ok
validate_project_id("")?;            // Err: must not be empty
```
