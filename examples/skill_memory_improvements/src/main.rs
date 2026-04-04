//! Validation example for the skill-memory-improvements feature.
//!
//! Exercises every new API added by the spec:
//! - Memory trait `add()` / `delete()` (adk-core)
//! - MemoryService `add_entry()` / `delete_entries()` (adk-memory)
//! - MemoryServiceAdapter forwarding
//! - Multi-directory skill discovery (`.skills/` + `.claude/skills/`)
//! - `discover_skill_files_with_extras` / `discover_instruction_files_with_extras`
//! - SkillInjectorConfig `global_skills_dir` / `extra_paths`
//! - Project-local skill precedence over global skills
//! - `triggers` field round-trip through parse
//! - Runner `with_auto_skills_mut`

use adk_core::{Content, Memory, MemoryEntry};
use adk_memory::{
    InMemoryMemoryService, MemoryServiceAdapter, MemoryService, SearchRequest,
};
use adk_skill::{
    SkillInjectorConfig, SkillInjector,
    discover_skill_files, discover_skill_files_with_extras,
    discover_instruction_files_with_extras,
    load_skill_index, load_skill_index_with_extras,
    parse_skill_markdown,
};
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut passed = 0u32;
        let mut failed = 0u32;

        macro_rules! check {
            ($name:expr, $body:expr) => {{
                match (|| -> Result<(), Box<dyn std::error::Error>> { $body; Ok(()) })() {
                    Ok(()) => { passed += 1; println!("  ✓ {}", $name); }
                    Err(e) => { failed += 1; println!("  ✗ {} — {e}", $name); }
                }
            }};
        }

        println!("\n=== Memory Trait: add / delete ===\n");
        validate_memory_add_delete(&mut passed, &mut failed).await;

        println!("\n=== MemoryService: add_entry / delete_entries ===\n");
        validate_memory_service_add_delete(&mut passed, &mut failed).await;

        println!("\n=== MemoryServiceAdapter forwarding ===\n");
        validate_adapter_forwarding(&mut passed, &mut failed).await;

        println!("\n=== Skill Discovery: .claude/skills/ ===\n");
        {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();

            check!("empty root returns empty list", {
                let files = discover_skill_files(root)?;
                assert!(files.is_empty(), "expected empty, got {}", files.len());
            });

            // Create .skills/ and .claude/skills/
            std::fs::create_dir_all(root.join(".skills")).unwrap();
            std::fs::create_dir_all(root.join(".claude/skills")).unwrap();
            std::fs::write(
                root.join(".skills/alpha.md"),
                "---\nname: alpha\ndescription: Alpha skill\n---\nAlpha body.",
            ).unwrap();
            std::fs::write(
                root.join(".claude/skills/beta.md"),
                "---\nname: beta\ndescription: Beta skill\n---\nBeta body.",
            ).unwrap();

            check!("discovers from both .skills/ and .claude/skills/", {
                let files = discover_skill_files(root)?;
                assert_eq!(files.len(), 2, "expected 2 files, got {}", files.len());
                assert!(files.iter().any(|p| p.ends_with("alpha.md")));
                assert!(files.iter().any(|p| p.ends_with("beta.md")));
            });

            check!("results are sorted", {
                let files = discover_skill_files(root)?;
                for w in files.windows(2) {
                    assert!(w[0] <= w[1], "not sorted: {:?} > {:?}", w[0], w[1]);
                }
            });
        }

        println!("\n=== Skill Discovery: with_extras ===\n");
        {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let extra = tempfile::tempdir().unwrap();

            std::fs::create_dir_all(root.join(".skills")).unwrap();
            std::fs::write(
                root.join(".skills/local.md"),
                "---\nname: local\ndescription: Local skill\n---\nLocal.",
            ).unwrap();
            std::fs::write(
                extra.path().join("global.md"),
                "---\nname: global\ndescription: Global skill\n---\nGlobal.",
            ).unwrap();

            check!("discover_skill_files_with_extras merges local + extra", {
                let files = discover_skill_files_with_extras(
                    root, &[extra.path().to_path_buf()],
                )?;
                assert_eq!(files.len(), 2);
            });

            check!("discover_instruction_files_with_extras includes extras", {
                let files = discover_instruction_files_with_extras(
                    root, &[extra.path().to_path_buf()],
                )?;
                assert!(files.len() >= 2);
            });

            check!("non-existent extra dir is silently skipped", {
                let files = discover_skill_files_with_extras(
                    root, &[PathBuf::from("/nonexistent/path")],
                )?;
                assert_eq!(files.len(), 1); // only local
            });
        }

        println!("\n=== SkillInjectorConfig: global_skills_dir / extra_paths ===\n");
        {
            check!("default config has None global and empty extras", {
                let cfg = SkillInjectorConfig::default();
                assert!(cfg.global_skills_dir.is_none());
                assert!(cfg.extra_paths.is_empty());
            });

            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let global = tempfile::tempdir().unwrap();

            std::fs::create_dir_all(root.join(".skills")).unwrap();
            std::fs::write(
                root.join(".skills/search.md"),
                "---\nname: search\ndescription: Search code\n---\nUse rg.",
            ).unwrap();
            std::fs::write(
                global.path().join("lint.md"),
                "---\nname: lint\ndescription: Lint code\n---\nRun clippy.",
            ).unwrap();

            check!("SkillInjector loads from root + global_skills_dir", {
                let cfg = SkillInjectorConfig {
                    global_skills_dir: Some(global.path().to_path_buf()),
                    ..SkillInjectorConfig::default()
                };
                let injector = SkillInjector::from_root(root, cfg)?;
                assert_eq!(injector.index().len(), 2);
                assert!(injector.index().find_by_name("search").is_some());
                assert!(injector.index().find_by_name("lint").is_some());
            });
        }

        println!("\n=== Project-local skill precedence ===\n");
        {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let global = tempfile::tempdir().unwrap();

            std::fs::create_dir_all(root.join(".skills")).unwrap();
            std::fs::write(
                root.join(".skills/search.md"),
                "---\nname: search\ndescription: Local search\n---\nLocal body.",
            ).unwrap();
            std::fs::write(
                global.path().join("search.md"),
                "---\nname: search\ndescription: Global search\n---\nGlobal body.",
            ).unwrap();

            check!("project-local skill wins over global with same name", {
                let index = load_skill_index_with_extras(
                    root, &[global.path().to_path_buf()],
                )?;
                let search_skills: Vec<_> = index.skills().iter()
                    .filter(|s| s.name == "search")
                    .collect();
                assert_eq!(search_skills.len(), 1);
                assert_eq!(search_skills[0].description, "Local search");
            });
        }

        println!("\n=== Triggers field round-trip ===\n");
        {
            let temp = tempfile::tempdir().unwrap();
            let skill_path = temp.path().join("test.md");
            let content = "\
---
name: rust-helper
description: Helps with Rust
triggers:
  - \"*.rs\"
  - Cargo.toml
  - \"*.toml\"
---
Body content here.";
            std::fs::write(&skill_path, content).unwrap();

            check!("triggers parsed from frontmatter", {
                let parsed = parse_skill_markdown(&skill_path, content)?;
                assert_eq!(parsed.triggers, vec!["*.rs", "Cargo.toml", "*.toml"]);
            });

            check!("trigger and triggers coexist independently", {
                let content2 = "\
---
name: dual
description: Both fields
trigger: true
triggers:
  - \"*.py\"
---
Body.";
                let parsed = parse_skill_markdown(
                    &temp.path().join("dual.md"), content2,
                )?;
                assert!(parsed.trigger);
                assert_eq!(parsed.triggers, vec!["*.py"]);
            });

            check!("triggers defaults to empty when absent", {
                let content3 = "\
---
name: notriggers
description: No triggers
---
Body.";
                let parsed = parse_skill_markdown(
                    &temp.path().join("no.md"), content3,
                )?;
                assert!(parsed.triggers.is_empty());
            });

            check!("triggers propagated through index", {
                let root = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(root.path().join(".skills")).unwrap();
                std::fs::write(
                    root.path().join(".skills/rs.md"),
                    "---\nname: rs\ndescription: Rust\ntriggers:\n  - \"*.rs\"\n---\nBody.",
                ).unwrap();
                let index = load_skill_index(root.path())?;
                let doc = index.find_by_name("rs").unwrap();
                assert_eq!(doc.triggers, vec!["*.rs"]);
                let summaries = index.summaries();
                let summary = summaries.iter().find(|s| s.name == "rs").unwrap();
                assert_eq!(summary.triggers, vec!["*.rs"]);
            });
        }

        println!("\n=== Runner: with_auto_skills_mut ===\n");
        validate_runner_mut(&mut passed, &mut failed).await;

        // Summary
        println!("\n============================================================");
        println!("  Results: {passed} passed, {failed} failed");
        if failed > 0 {
            std::process::exit(1);
        } else {
            println!("  All validations passed.");
        }
    });
}


async fn validate_memory_add_delete(passed: &mut u32, failed: &mut u32) {
    // Test via MemoryServiceAdapter (which implements adk_core::Memory)
    let service = Arc::new(InMemoryMemoryService::new());
    let adapter = Arc::new(MemoryServiceAdapter::new(
        service.clone(), "test-app", "user-1",
    ));

    // add() via Memory trait
    let entry = MemoryEntry {
        content: Content::new("user").with_text("Rust is a systems programming language"),
        author: "user".to_string(),
    };
    match adapter.add(entry).await {
        Ok(()) => { *passed += 1; println!("  ✓ Memory::add succeeds"); }
        Err(e) => { *failed += 1; println!("  ✗ Memory::add — {e}"); }
    }

    // search() via Memory trait
    match adapter.search("Rust programming").await {
        Ok(results) => {
            if results.is_empty() {
                *failed += 1;
                println!("  ✗ Memory::search after add — empty results");
            } else {
                *passed += 1;
                println!("  ✓ Memory::search after add returns {} result(s)", results.len());
            }
        }
        Err(e) => { *failed += 1; println!("  ✗ Memory::search — {e}"); }
    }

    // delete() via Memory trait
    match adapter.delete("Rust").await {
        Ok(count) => {
            if count > 0 {
                *passed += 1;
                println!("  ✓ Memory::delete removed {count} entry(ies)");
            } else {
                *failed += 1;
                println!("  ✗ Memory::delete returned 0");
            }
        }
        Err(e) => { *failed += 1; println!("  ✗ Memory::delete — {e}"); }
    }

    // search after delete should be empty
    match adapter.search("Rust programming").await {
        Ok(results) => {
            if results.is_empty() {
                *passed += 1;
                println!("  ✓ Memory::search after delete returns empty");
            } else {
                *failed += 1;
                println!("  ✗ Memory::search after delete — got {} result(s)", results.len());
            }
        }
        Err(e) => { *failed += 1; println!("  ✗ Memory::search after delete — {e}"); }
    }

    // Default Memory impl returns "not implemented"
    struct ReadOnlyMemory;
    #[async_trait::async_trait]
    impl Memory for ReadOnlyMemory {
        async fn search(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
    }
    let ro = ReadOnlyMemory;
    match ro.add(MemoryEntry {
        content: Content::new("user").with_text("test"),
        author: "test".to_string(),
    }).await {
        Err(e) if e.to_string().contains("not implemented") => {
            *passed += 1;
            println!("  ✓ default Memory::add returns 'not implemented'");
        }
        other => {
            *failed += 1;
            println!("  ✗ default Memory::add — unexpected: {other:?}");
        }
    }
    match ro.delete("test").await {
        Err(e) if e.to_string().contains("not implemented") => {
            *passed += 1;
            println!("  ✓ default Memory::delete returns 'not implemented'");
        }
        other => {
            *failed += 1;
            println!("  ✗ default Memory::delete — unexpected: {other:?}");
        }
    }
}

async fn validate_memory_service_add_delete(passed: &mut u32, failed: &mut u32) {
    let service = InMemoryMemoryService::new();

    // add_entry
    let entry = adk_memory::MemoryEntry {
        content: Content::new("user").with_text("Tokyo is the capital of Japan"),
        author: "user".to_string(),
        timestamp: chrono::Utc::now(),
    };
    match service.add_entry("app", "user1", entry).await {
        Ok(()) => { *passed += 1; println!("  ✓ add_entry succeeds"); }
        Err(e) => { *failed += 1; println!("  ✗ add_entry — {e}"); }
    }

    // search
    match service.search(SearchRequest {
        query: "Tokyo capital".to_string(),
        app_name: "app".to_string(),
        user_id: "user1".to_string(),
        limit: None,
        min_score: None,
    }).await {
        Ok(resp) => {
            if resp.memories.is_empty() {
                *failed += 1;
                println!("  ✗ search after add_entry — empty");
            } else {
                *passed += 1;
                println!("  ✓ search after add_entry returns {} result(s)", resp.memories.len());
            }
        }
        Err(e) => { *failed += 1; println!("  ✗ search — {e}"); }
    }

    // delete_entries
    match service.delete_entries("app", "user1", "Tokyo").await {
        Ok(count) => {
            if count > 0 {
                *passed += 1;
                println!("  ✓ delete_entries removed {count} entry(ies)");
            } else {
                *failed += 1;
                println!("  ✗ delete_entries returned 0");
            }
        }
        Err(e) => { *failed += 1; println!("  ✗ delete_entries — {e}"); }
    }

    // search after delete
    match service.search(SearchRequest {
        query: "Tokyo".to_string(),
        app_name: "app".to_string(),
        user_id: "user1".to_string(),
        limit: None,
        min_score: None,
    }).await {
        Ok(resp) => {
            if resp.memories.is_empty() {
                *passed += 1;
                println!("  ✓ search after delete_entries returns empty");
            } else {
                *failed += 1;
                println!("  ✗ search after delete — got {} result(s)", resp.memories.len());
            }
        }
        Err(e) => { *failed += 1; println!("  ✗ search after delete — {e}"); }
    }
}

async fn validate_adapter_forwarding(passed: &mut u32, failed: &mut u32) {
    let service = Arc::new(InMemoryMemoryService::new());
    let adapter = MemoryServiceAdapter::new(service.clone(), "app", "user");

    // Add via adapter (Memory trait)
    let entry = MemoryEntry {
        content: Content::new("user").with_text("adapter forwarding test data"),
        author: "system".to_string(),
    };
    adapter.add(entry).await.unwrap();

    // Search via adapter
    let results = adapter.search("forwarding test").await.unwrap();
    if results.is_empty() {
        *failed += 1;
        println!("  ✗ adapter search after add — empty");
    } else {
        *passed += 1;
        println!("  ✓ adapter forwards add → add_entry correctly");
    }

    // Search directly on service to confirm data landed
    let direct = service.search(SearchRequest {
        query: "forwarding test".to_string(),
        app_name: "app".to_string(),
        user_id: "user".to_string(),
        limit: None,
        min_score: None,
    }).await.unwrap();
    if direct.memories.is_empty() {
        *failed += 1;
        println!("  ✗ direct service search — empty");
    } else {
        *passed += 1;
        println!("  ✓ adapter-added data visible via direct service search");
    }

    // Delete via adapter
    let deleted = adapter.delete("forwarding").await.unwrap();
    if deleted > 0 {
        *passed += 1;
        println!("  ✓ adapter forwards delete → delete_entries ({deleted} removed)");
    } else {
        *failed += 1;
        println!("  ✗ adapter delete returned 0");
    }
}

async fn validate_runner_mut(passed: &mut u32, failed: &mut u32) {
    use adk_runner::{Runner, RunnerConfig};
    use adk_session::InMemorySessionService;
    use adk_core::{Agent, InvocationContext, Event};
    use futures::stream::BoxStream;

    // Minimal agent for Runner construction
    struct NoopAgent;
    #[async_trait::async_trait]
    impl Agent for NoopAgent {
        fn name(&self) -> &str { "noop" }
        fn description(&self) -> &str { "noop agent" }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] { &[] }
        async fn run(
            &self, _ctx: Arc<dyn InvocationContext>,
        ) -> adk_core::Result<BoxStream<'static, adk_core::Result<Event>>> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".skills")).unwrap();
    std::fs::write(
        root.join(".skills/test.md"),
        "---\nname: test-skill\ndescription: Test\n---\nTest body.",
    ).unwrap();

    // Build runner
    #[allow(deprecated)]
    let mut runner = Runner::new(RunnerConfig {
        app_name: "test".to_string(),
        agent: Arc::new(NoopAgent),
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    }).unwrap();

    // with_auto_skills_mut borrows mutably
    match runner.with_auto_skills_mut(root, SkillInjectorConfig::default()) {
        Ok(()) => {
            *passed += 1;
            println!("  ✓ with_auto_skills_mut succeeds");
        }
        Err(e) => {
            *failed += 1;
            println!("  ✗ with_auto_skills_mut — {e}");
        }
    }

    // Runner is still usable after the call (not consumed)
    // Prove it by calling with_auto_skills_mut again
    match runner.with_auto_skills_mut(root, SkillInjectorConfig::default()) {
        Ok(()) => {
            *passed += 1;
            println!("  ✓ Runner still usable after with_auto_skills_mut (called twice)");
        }
        Err(e) => {
            *failed += 1;
            println!("  ✗ Runner not usable after with_auto_skills_mut — {e}");
        }
    }

    // Error path: non-existent root with .skills/ that is a file
    let bad_root = tempfile::tempdir().unwrap();
    // with_auto_skills_mut on empty dir should succeed (empty index)
    match runner.with_auto_skills_mut(bad_root.path(), SkillInjectorConfig::default()) {
        Ok(()) => {
            *passed += 1;
            println!("  ✓ with_auto_skills_mut on empty dir succeeds (empty index)");
        }
        Err(e) => {
            *failed += 1;
            println!("  ✗ with_auto_skills_mut on empty dir — {e}");
        }
    }
}
