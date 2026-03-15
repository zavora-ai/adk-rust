//! AgentSkills example: Tier 3 skill coordination with "Verified Toolpath".
//!
//! Demonstrates the `ContextCoordinator` orchestrating skill selection,
//! tool validation, and context engineering.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_coordinator

use adk_core::{Result, Tool, ToolContext, ToolRegistry, ValidationMode};
use adk_skill::{
    ContextCoordinator, CoordinatorConfig, ResolutionStrategy, SelectionPolicy, load_skill_index,
};
use anyhow::anyhow;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// --- Mock Tooling ---

struct WeatherTool;
#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "weather"
    }
    fn description(&self) -> &str {
        "Get the current weather."
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(Value::String("Sunny, 25°C".into()))
    }
}

struct SearchTool;
#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "knowledge"
    }
    fn description(&self) -> &str {
        "Search the knowledge base."
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(Value::String("Found 3 matches for your query.".into()))
    }
}

struct MyToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry for MyToolRegistry {
    fn resolve(&self, tool_name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(tool_name).cloned()
    }

    fn available_tools(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

// --- Demo Setup ---

fn setup_demo_skills() -> anyhow::Result<PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_coordinator_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    let skills_dir = root.join(".skills");
    std::fs::create_dir_all(&skills_dir)?;

    std::fs::write(
        skills_dir.join("weather_bot.md"),
        r#"---
name: weather_bot
description: Helpful weather assistant.
allowed-tools: [weather]
---
I am the weather bot. I will help you with forecasts.
"#,
    )?;

    std::fs::write(
        skills_dir.join("researcher.md"),
        r#"---
name: researcher
description: Advanced deep-dive researcher.
allowed-tools: [knowledge]
tags: [advanced]
---
I perform deep research using the knowledge base.
"#,
    )?;

    std::fs::write(
        skills_dir.join("broken_skill.md"),
        r#"---
name: broken_skill
description: Skill requesting a non-existent tool.
allowed-tools: [phantom_tool]
---
I am broken.
"#,
    )?;

    Ok(root)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let skills_root = setup_demo_skills()?;
    let index = Arc::new(load_skill_index(&skills_root)?);

    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    tools.insert("weather".into(), Arc::new(WeatherTool));
    tools.insert("knowledge".into(), Arc::new(SearchTool));
    let registry = Arc::new(MyToolRegistry { tools });

    println!("--- Tier 3 Skill Coordination Demo ---\n");

    // 1. Success Case: Permissive Resolution
    let coordinator = ContextCoordinator::new(
        index.clone(),
        registry.clone(),
        CoordinatorConfig { validation_mode: ValidationMode::Permissive, ..Default::default() },
    );

    let strategies = vec![ResolutionStrategy::ByQuery("What's the weather like today?".into())];

    println!("Resolving intent: 'What's the weather like today?'");
    if let Some(ctx) = coordinator.resolve(&strategies) {
        println!("Match Found: {}", ctx.provenance.skill.name);
        println!(
            "Tools Bound: {:?}",
            ctx.active_tools.iter().map(|t| t.name()).collect::<Vec<_>>()
        );
        println!("System Instruction:\n{}\n", ctx.inner.system_instruction);
    }

    // 2. Success Case: Multi-Strategy Resolution
    let strategies = vec![
        ResolutionStrategy::ByName("nonexistent".into()), // Should skip
        ResolutionStrategy::ByTag("advanced".into()),     // Should match researcher
    ];

    println!("Resolving multi-strategy: [ByName('nonexistent'), ByTag('advanced')]");
    if let Some(ctx) = coordinator.resolve(&strategies) {
        println!("Match Found: {}", ctx.provenance.skill.name);
        println!(
            "Tools Bound: {:?}\n",
            ctx.active_tools.iter().map(|t| t.name()).collect::<Vec<_>>()
        );
    }

    // 3. Failure Case: Strict Validation
    let coordinator_strict = ContextCoordinator::new(
        index.clone(),
        registry.clone(),
        CoordinatorConfig {
            validation_mode: ValidationMode::Strict,
            policy: SelectionPolicy { min_score: 0.0, ..Default::default() }, // Force match broken skill
            ..Default::default()
        },
    );

    let strategies = vec![ResolutionStrategy::ByName("broken_skill".into())];

    println!("Resolving 'broken_skill' in Strict mode...");
    // Since resolve() uses try_resolve().ok(), it returns None on validation failure.
    let result = coordinator_strict.resolve(&strategies);

    if result.is_none() {
        println!("Correctly rejected broken skill in Strict mode (resolution returned None).");
    } else {
        return Err(anyhow!("Error: Should have rejected skill with missing tool in Strict mode"));
    }

    Ok(())
}
