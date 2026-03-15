//! AgentSkills example: zero-latency, purely lexical discovery.
//!
//! Demonstrates how to discover and score skill files (including conventions
//! like AGENTS.md and GEMINI.md) without making any LLM or network calls.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_discovery

use adk_skill::{SelectionPolicy, load_skill_index, select_skills};
use anyhow::Result;
use std::path::PathBuf;

fn setup_demo_root() -> Result<PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_discovery_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(root.join(".skills"))?;

    std::fs::write(
        root.join(".skills/code_search.md"),
        "---
name: code_search
description: Search repository source code
tags: [code, search]
---
Use rg --files, then rg <pattern>.
",
    )?;
    std::fs::write(
        root.join("AGENTS.md"),
        "# Repository Agent Rules
Always run cargo test before commit.
",
    )?;
    std::fs::write(
        root.join("GEMINI.md"),
        "# Gemini Guidance
Use default to gemini-2.5-flash.
",
    )?;

    Ok(root)
}

fn main() -> Result<()> {
    let root = setup_demo_root()?;

    // 1. Discovery: scans root and .skills/ for markdown files
    let index = load_skill_index(&root)?;

    println!(
        "--- Pure Lexical Discovery Demo ---
"
    );
    println!(
        "Discovered {} instruction files:
",
        index.len()
    );
    for skill in index.summaries() {
        println!("- {:<15} | tags={:?} | path={}", skill.name, skill.tags, skill.path.display());
    }

    // 2. Scoring: runs in microseconds without LLM
    let policy = SelectionPolicy { top_k: 1, min_score: 0.1, ..SelectionPolicy::default() };
    let queries = [
        "how should we configure gemini for this repo",
        "what are the repository test rules",
        "find todo markers in code",
    ];

    println!(
        "
Top lexical match per query:"
    );
    for query in queries {
        let matches = select_skills(&index, query, &policy);
        if let Some(top) = matches.first() {
            println!("* {:?} -> {} (score: {:.2})", query, top.skill.name, top.score);
        } else {
            println!("* {:?} -> no match", query);
        }
    }

    Ok(())
}
