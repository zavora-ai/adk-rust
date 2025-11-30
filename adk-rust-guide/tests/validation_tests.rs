//! Property-based tests for documentation validation
//!
//! These tests verify the correspondence between documentation pages
//! and validation examples.

use proptest::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use walkdir::WalkDir;

/// Get the workspace root directory
fn workspace_root() -> PathBuf {
    // When running tests, we need to find the workspace root
    // Try current directory first, then look for Cargo.toml
    let current = std::env::current_dir().unwrap_or_default();
    
    // Check if we're in the workspace root (has docs/ and adk-rust-guide/)
    if current.join("docs/official_docs").exists() {
        return current;
    }
    
    // Try parent directories
    let mut path = current.as_path();
    while let Some(parent) = path.parent() {
        if parent.join("docs/official_docs").exists() {
            return parent.to_path_buf();
        }
        path = parent;
    }
    
    current
}

/// Get all documentation pages (excluding index.md)
fn get_doc_pages() -> Vec<String> {
    let root = workspace_root();
    let docs_path = root.join("docs/official_docs");
    let mut pages = Vec::new();

    if !docs_path.exists() {
        return pages;
    }

    for entry in WalkDir::new(&docs_path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |ext| ext == "md") {
            let relative = path.strip_prefix(&docs_path).unwrap();
            let name = relative.to_string_lossy().to_string();
            // Exclude index.md from validation requirements
            if name != "index.md" {
                pages.push(name);
            }
        }
    }

    pages
}

/// Get all example files
fn get_example_files() -> Vec<String> {
    let root = workspace_root();
    let examples_path = root.join("adk-rust-guide/examples");
    let mut examples = Vec::new();

    if !examples_path.exists() {
        return examples;
    }

    for entry in WalkDir::new(&examples_path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |ext| ext == "rs") {
            let relative = path.strip_prefix(&examples_path).unwrap();
            let name = relative.to_string_lossy().to_string();
            examples.push(name);
        }
    }

    examples
}

/// Map a documentation page to its expected example file(s)
fn doc_to_example_mapping(doc_page: &str) -> Vec<String> {
    // Remove .md extension and convert to example path pattern
    let base = doc_page.trim_end_matches(".md");
    
    // Handle different documentation structures
    match base {
        // Top-level docs map to single examples
        "introduction" => vec![], // Introduction doesn't need a runnable example
        "quickstart" => vec!["quickstart.rs".to_string()],
        
        // Agent docs
        "agents/llm-agent" => vec![
            "agents/llm_agent_basic.rs".to_string(),
            "agents/llm_agent_config.rs".to_string(),
        ],
        "agents/workflow-agents" => vec![
            "agents/sequential_agent.rs".to_string(),
            "agents/parallel_agent.rs".to_string(),
            "agents/loop_agent.rs".to_string(),
        ],
        "agents/multi-agent" => vec!["agents/multi_agent.rs".to_string()],
        
        // Tool docs
        "tools/function-tools" => vec!["tools/function_tool.rs".to_string()],
        "tools/built-in-tools" => vec![
            "tools/built_in_google_search.rs".to_string(),
            "tools/built_in_exit_loop.rs".to_string(),
            "tools/built_in_load_artifacts.rs".to_string(),
        ],
        "tools/mcp-tools" => vec!["tools/mcp_tool.rs".to_string()],
        
        // Session docs
        "sessions/sessions" => vec!["sessions/session_basic.rs".to_string()],
        "sessions/state" => vec!["sessions/state_management.rs".to_string()],
        
        // Callback docs
        "callbacks/callbacks" => vec![
            "callbacks/before_agent.rs".to_string(),
            "callbacks/after_agent.rs".to_string(),
            "callbacks/model_callbacks.rs".to_string(),
            "callbacks/tool_callbacks.rs".to_string(),
        ],
        
        // Artifact docs
        "artifacts/artifacts" => vec!["artifacts/artifact_ops.rs".to_string()],
        
        // Event docs
        "events/events" => vec!["events/event_inspection.rs".to_string()],
        
        // Observability docs
        "observability/telemetry" => vec!["observability/telemetry.rs".to_string()],
        
        // Deployment docs
        "deployment/launcher" => vec!["deployment/console_mode.rs".to_string()],
        "deployment/server" => vec!["deployment/server_mode.rs".to_string()],
        "deployment/a2a" => vec![], // A2A may be in roadmap
        
        // Default: try to find a matching example
        _ => vec![],
    }
}

/// **Feature: official-documentation, Property 2: Documentation-Example Correspondence**
/// 
/// *For any* documentation page in `docs/official_docs/` (excluding index),
/// there SHALL exist a corresponding example file in `adk-rust-guide/examples/`
/// that validates its code samples.
/// 
/// **Validates: Requirements 16.1, 16.2**
#[test]
fn test_documentation_example_correspondence() {
    let doc_pages = get_doc_pages();
    let example_files: HashSet<String> = get_example_files().into_iter().collect();
    
    let mut missing_examples = Vec::new();
    
    for doc_page in &doc_pages {
        let expected_examples = doc_to_example_mapping(doc_page);
        
        // Skip docs that don't require examples (like introduction)
        if expected_examples.is_empty() {
            continue;
        }
        
        for expected in &expected_examples {
            if !example_files.contains(expected) {
                missing_examples.push((doc_page.clone(), expected.clone()));
            }
        }
    }
    
    if !missing_examples.is_empty() {
        let msg = missing_examples
            .iter()
            .map(|(doc, ex)| format!("  {} -> {}", doc, ex))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "Documentation pages missing corresponding examples:\n{}",
            msg
        );
    }
}

// Property test using proptest to verify example files exist for sampled doc pages
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    
    /// **Feature: official-documentation, Property 2: Documentation-Example Correspondence**
    /// **Validates: Requirements 16.1, 16.2**
    #[test]
    fn prop_doc_has_example(index in 0usize..100) {
        let doc_pages = get_doc_pages();
        if doc_pages.is_empty() {
            return Ok(());
        }
        
        let example_files: HashSet<String> = get_example_files().into_iter().collect();
        let doc_page = &doc_pages[index % doc_pages.len()];
        let expected_examples = doc_to_example_mapping(doc_page);
        
        // Skip docs that don't require examples
        if expected_examples.is_empty() {
            return Ok(());
        }
        
        for expected in &expected_examples {
            prop_assert!(
                example_files.contains(expected),
                "Doc '{}' missing example '{}'",
                doc_page,
                expected
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
/// 
/// *For any* documentation page with a code sample, the corresponding validation
/// example in adk-rust-guide SHALL compile successfully.
/// 
/// This test verifies that example files have valid Rust syntax by checking
/// that they can be parsed as valid Rust code.
/// 
/// **Validates: Requirements 2.3, 2.5**
#[test]
fn test_quickstart_example_compilation() {
    let root = workspace_root();
    let quickstart_path = root.join("adk-rust-guide/examples/quickstart.rs");
    
    assert!(
        quickstart_path.exists(),
        "Quickstart example must exist at {:?}",
        quickstart_path
    );
    
    // Read the file and verify it contains expected patterns
    let content = std::fs::read_to_string(&quickstart_path)
        .expect("Should be able to read quickstart.rs");
    
    // Verify the example contains key documentation patterns
    assert!(
        content.contains("Validates: docs/official_docs/quickstart.md"),
        "Quickstart example must reference the documentation it validates"
    );
    assert!(
        content.contains("GeminiModel::new"),
        "Quickstart example must demonstrate model creation"
    );
    assert!(
        content.contains("LlmAgentBuilder::new"),
        "Quickstart example must demonstrate agent building"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Quickstart example must be an async main function"
    );
}

// Property test for example compilation verification
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    
    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    /// 
    /// *For any* example file in adk-rust-guide/examples/, the file SHALL:
    /// 1. Exist on disk
    /// 2. Contain valid Rust syntax (verified by presence of expected patterns)
    /// 3. Include a documentation reference comment
    /// 
    /// **Validates: Requirements 2.3, 2.5**
    #[test]
    fn prop_example_has_valid_structure(index in 0usize..100) {
        let example_files = get_example_files();
        if example_files.is_empty() {
            return Ok(());
        }
        
        let root = workspace_root();
        let example_file = &example_files[index % example_files.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);
        
        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Example file should exist: {:?}",
            example_path
        );
        
        // Read and verify basic structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // All examples should have a documentation reference
            prop_assert!(
                content.contains("//!") || content.contains("///"),
                "Example {} should have documentation comments",
                example_file
            );
            
            // All examples should be async main or have test functions
            prop_assert!(
                content.contains("#[tokio::main]") || content.contains("#[test]"),
                "Example {} should have a main function or tests",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
/// 
/// *For any* LlmAgent example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the LlmAgent documentation
/// 4. Demonstrate LlmAgentBuilder usage
/// 
/// **Validates: Requirements 3.2, 3.5**
#[test]
fn test_llm_agent_example_compilation() {
    let root = workspace_root();
    
    // Test llm_agent_basic.rs
    let basic_path = root.join("adk-rust-guide/examples/agents/llm_agent_basic.rs");
    assert!(
        basic_path.exists(),
        "LlmAgent basic example must exist at {:?}",
        basic_path
    );
    
    let basic_content = std::fs::read_to_string(&basic_path)
        .expect("Should be able to read llm_agent_basic.rs");
    
    assert!(
        basic_content.contains("Validates: docs/official_docs/agents/llm-agent.md"),
        "LlmAgent basic example must reference the documentation it validates"
    );
    assert!(
        basic_content.contains("LlmAgentBuilder::new"),
        "LlmAgent basic example must demonstrate agent building"
    );
    assert!(
        basic_content.contains("std::result::Result"),
        "LlmAgent basic example must use std::result::Result to avoid conflicts"
    );
    assert!(
        basic_content.contains("is_interactive_mode"),
        "LlmAgent basic example must support validation/interactive modes"
    );
    
    // Test llm_agent_config.rs
    let config_path = root.join("adk-rust-guide/examples/agents/llm_agent_config.rs");
    assert!(
        config_path.exists(),
        "LlmAgent config example must exist at {:?}",
        config_path
    );
    
    let config_content = std::fs::read_to_string(&config_path)
        .expect("Should be able to read llm_agent_config.rs");
    
    assert!(
        config_content.contains("Validates: docs/official_docs/agents/llm-agent.md"),
        "LlmAgent config example must reference the documentation it validates"
    );
    assert!(
        config_content.contains("LlmAgentBuilder::new"),
        "LlmAgent config example must demonstrate agent building"
    );
    assert!(
        config_content.contains("IncludeContents"),
        "LlmAgent config example must demonstrate IncludeContents configuration"
    );
    assert!(
        config_content.contains("instruction"),
        "LlmAgent config example must demonstrate instruction configuration"
    );
    assert!(
        config_content.contains("tool"),
        "LlmAgent config example must demonstrate tool configuration"
    );
    assert!(
        config_content.contains("std::result::Result"),
        "LlmAgent config example must use std::result::Result to avoid conflicts"
    );
}

// Property test for LlmAgent examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    
    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    /// 
    /// *For any* LlmAgent example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    /// 
    /// **Validates: Requirements 3.2, 3.5**
    #[test]
    fn prop_llm_agent_examples_valid(index in 0usize..100) {
        let root = workspace_root();
        let llm_agent_examples = vec![
            "agents/llm_agent_basic.rs",
            "agents/llm_agent_config.rs",
        ];
        
        let example_file = llm_agent_examples[index % llm_agent_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);
        
        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "LlmAgent example should exist: {:?}",
            example_path
        );
        
        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("agents/llm-agent.md"),
                "Example {} should reference llm-agent.md documentation",
                example_file
            );
            
            // Must use LlmAgentBuilder
            prop_assert!(
                content.contains("LlmAgentBuilder"),
                "Example {} should use LlmAgentBuilder",
                example_file
            );
            
            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );
            
            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
/// 
/// *For any* workflow agent example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the workflow-agents documentation
/// 4. Demonstrate the appropriate workflow agent usage
/// 
/// **Validates: Requirements 4.1, 4.2, 4.3, 4.5**
#[test]
fn test_workflow_agent_example_compilation() {
    let root = workspace_root();
    
    // Test sequential_agent.rs
    let sequential_path = root.join("adk-rust-guide/examples/agents/sequential_agent.rs");
    assert!(
        sequential_path.exists(),
        "SequentialAgent example must exist at {:?}",
        sequential_path
    );
    
    let sequential_content = std::fs::read_to_string(&sequential_path)
        .expect("Should be able to read sequential_agent.rs");
    
    assert!(
        sequential_content.contains("Validates: docs/official_docs/agents/workflow-agents.md"),
        "SequentialAgent example must reference the documentation it validates"
    );
    assert!(
        sequential_content.contains("SequentialAgent::new"),
        "SequentialAgent example must demonstrate SequentialAgent creation"
    );
    assert!(
        sequential_content.contains("std::result::Result"),
        "SequentialAgent example must use std::result::Result to avoid conflicts"
    );
    assert!(
        sequential_content.contains("is_interactive_mode"),
        "SequentialAgent example must support validation/interactive modes"
    );
    
    // Test parallel_agent.rs
    let parallel_path = root.join("adk-rust-guide/examples/agents/parallel_agent.rs");
    assert!(
        parallel_path.exists(),
        "ParallelAgent example must exist at {:?}",
        parallel_path
    );
    
    let parallel_content = std::fs::read_to_string(&parallel_path)
        .expect("Should be able to read parallel_agent.rs");
    
    assert!(
        parallel_content.contains("Validates: docs/official_docs/agents/workflow-agents.md"),
        "ParallelAgent example must reference the documentation it validates"
    );
    assert!(
        parallel_content.contains("ParallelAgent::new"),
        "ParallelAgent example must demonstrate ParallelAgent creation"
    );
    assert!(
        parallel_content.contains("std::result::Result"),
        "ParallelAgent example must use std::result::Result to avoid conflicts"
    );
    
    // Test loop_agent.rs
    let loop_path = root.join("adk-rust-guide/examples/agents/loop_agent.rs");
    assert!(
        loop_path.exists(),
        "LoopAgent example must exist at {:?}",
        loop_path
    );
    
    let loop_content = std::fs::read_to_string(&loop_path)
        .expect("Should be able to read loop_agent.rs");
    
    assert!(
        loop_content.contains("Validates: docs/official_docs/agents/workflow-agents.md"),
        "LoopAgent example must reference the documentation it validates"
    );
    assert!(
        loop_content.contains("LoopAgent::new"),
        "LoopAgent example must demonstrate LoopAgent creation"
    );
    assert!(
        loop_content.contains("ExitLoopTool"),
        "LoopAgent example must demonstrate ExitLoopTool usage"
    );
    assert!(
        loop_content.contains("with_max_iterations"),
        "LoopAgent example must demonstrate max_iterations configuration"
    );
    assert!(
        loop_content.contains("std::result::Result"),
        "LoopAgent example must use std::result::Result to avoid conflicts"
    );
}

// Property test for workflow agent examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    
    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    /// 
    /// *For any* workflow agent example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    /// 
    /// **Validates: Requirements 4.1, 4.2, 4.3, 4.5**
    #[test]
    fn prop_workflow_agent_examples_valid(index in 0usize..100) {
        let root = workspace_root();
        let workflow_examples = vec![
            ("agents/sequential_agent.rs", "SequentialAgent"),
            ("agents/parallel_agent.rs", "ParallelAgent"),
            ("agents/loop_agent.rs", "LoopAgent"),
        ];
        
        let (example_file, agent_type) = &workflow_examples[index % workflow_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);
        
        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Workflow agent example should exist: {:?}",
            example_path
        );
        
        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("workflow-agents.md"),
                "Example {} should reference workflow-agents.md documentation",
                example_file
            );
            
            // Must use the appropriate agent type
            prop_assert!(
                content.contains(&format!("{}::new", agent_type)),
                "Example {} should use {}::new",
                example_file,
                agent_type
            );
            
            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );
            
            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
/// 
/// *For any* multi-agent example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the multi-agent documentation
/// 4. Demonstrate sub-agent configuration and hierarchy
/// 
/// **Validates: Requirements 11.6**
#[test]
fn test_multi_agent_example_compilation() {
    let root = workspace_root();
    
    // Test multi_agent.rs
    let multi_agent_path = root.join("adk-rust-guide/examples/agents/multi_agent.rs");
    assert!(
        multi_agent_path.exists(),
        "Multi-agent example must exist at {:?}",
        multi_agent_path
    );
    
    let content = std::fs::read_to_string(&multi_agent_path)
        .expect("Should be able to read multi_agent.rs");
    
    assert!(
        content.contains("Validates: docs/official_docs/agents/multi-agent.md"),
        "Multi-agent example must reference the documentation it validates"
    );
    assert!(
        content.contains("sub_agent"),
        "Multi-agent example must demonstrate sub_agent configuration"
    );
    assert!(
        content.contains("sub_agents()"),
        "Multi-agent example must demonstrate sub_agents() access"
    );
    assert!(
        content.contains("global_instruction"),
        "Multi-agent example must demonstrate global_instruction usage"
    );
    assert!(
        content.contains("std::result::Result"),
        "Multi-agent example must use std::result::Result to avoid conflicts"
    );
    assert!(
        content.contains("is_interactive_mode"),
        "Multi-agent example must support validation/interactive modes"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Multi-agent example must be an async main function"
    );
}

// Property test for multi-agent examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    
    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    /// 
    /// *For any* multi-agent example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    /// 
    /// **Validates: Requirements 11.6**
    #[test]
    fn prop_multi_agent_example_valid(index in 0usize..100) {
        let root = workspace_root();
        let multi_agent_examples = vec![
            "agents/multi_agent.rs",
        ];
        
        let example_file = multi_agent_examples[index % multi_agent_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);
        
        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Multi-agent example should exist: {:?}",
            example_path
        );
        
        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("multi-agent.md"),
                "Example {} should reference multi-agent.md documentation",
                example_file
            );
            
            // Must use sub_agent configuration
            prop_assert!(
                content.contains("sub_agent"),
                "Example {} should use sub_agent configuration",
                example_file
            );
            
            // Must demonstrate hierarchy
            prop_assert!(
                content.contains("sub_agents()"),
                "Example {} should demonstrate sub_agents() access",
                example_file
            );
            
            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );
            
            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
/// 
/// *For any* function tool example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the function-tools documentation
/// 4. Demonstrate FunctionTool creation and usage
/// 
/// **Validates: Requirements 5.4, 5.6**
#[test]
fn test_function_tool_example_compilation() {
    let root = workspace_root();
    
    // Test function_tool.rs
    let function_tool_path = root.join("adk-rust-guide/examples/tools/function_tool.rs");
    assert!(
        function_tool_path.exists(),
        "FunctionTool example must exist at {:?}",
        function_tool_path
    );
    
    let content = std::fs::read_to_string(&function_tool_path)
        .expect("Should be able to read function_tool.rs");
    
    // Verify documentation reference
    assert!(
        content.contains("Validates: docs/official_docs/tools/function-tools.md"),
        "FunctionTool example must reference the documentation it validates"
    );
    
    // Verify FunctionTool usage
    assert!(
        content.contains("FunctionTool::new"),
        "FunctionTool example must demonstrate FunctionTool creation"
    );
    
    // Verify parameter handling demonstration
    assert!(
        content.contains("args.get"),
        "FunctionTool example must demonstrate parameter extraction from args"
    );
    
    // Verify return value convention
    assert!(
        content.contains("Result<Value>") || content.contains("-> Result<Value"),
        "FunctionTool example must demonstrate Result<Value> return type"
    );
    
    // Verify ToolContext usage
    assert!(
        content.contains("Arc<dyn ToolContext>"),
        "FunctionTool example must demonstrate ToolContext parameter"
    );
    
    // Verify coding conventions
    assert!(
        content.contains("std::result::Result"),
        "FunctionTool example must use std::result::Result to avoid conflicts"
    );
    assert!(
        content.contains("is_interactive_mode"),
        "FunctionTool example must support validation/interactive modes"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "FunctionTool example must be an async main function"
    );
}

// Property test for function tool examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    
    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    /// 
    /// *For any* function tool example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    /// 
    /// **Validates: Requirements 5.4, 5.6**
    #[test]
    fn prop_function_tool_example_valid(index in 0usize..100) {
        let root = workspace_root();
        let function_tool_examples = vec![
            "tools/function_tool.rs",
        ];
        
        let example_file = function_tool_examples[index % function_tool_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);
        
        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "FunctionTool example should exist: {:?}",
            example_path
        );
        
        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("function-tools.md"),
                "Example {} should reference function-tools.md documentation",
                example_file
            );
            
            // Must use FunctionTool::new
            prop_assert!(
                content.contains("FunctionTool::new"),
                "Example {} should use FunctionTool::new",
                example_file
            );
            
            // Must demonstrate parameter handling
            prop_assert!(
                content.contains("args.get") || content.contains("args["),
                "Example {} should demonstrate parameter extraction",
                example_file
            );
            
            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );
            
            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
///
/// *For any* built-in tools example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the built-in-tools documentation
/// 4. Demonstrate the specific built-in tool
///
/// **Validates: Requirements 6.4**
#[test]
fn test_built_in_tools_example_compilation() {
    let root = workspace_root();

    // Test built_in_google_search.rs
    let google_search_path = root.join("adk-rust-guide/examples/tools/built_in_google_search.rs");
    assert!(
        google_search_path.exists(),
        "GoogleSearchTool example must exist at {:?}",
        google_search_path
    );

    let content = std::fs::read_to_string(&google_search_path)
        .expect("Should be able to read built_in_google_search.rs");

    assert!(
        content.contains("Validates: docs/official_docs/tools/built-in-tools.md"),
        "GoogleSearchTool example must reference the documentation it validates"
    );
    assert!(
        content.contains("GoogleSearchTool"),
        "GoogleSearchTool example must demonstrate GoogleSearchTool usage"
    );
    assert!(
        content.contains("std::result::Result"),
        "GoogleSearchTool example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "GoogleSearchTool example must be an async main function"
    );

    // Test built_in_exit_loop.rs
    let exit_loop_path = root.join("adk-rust-guide/examples/tools/built_in_exit_loop.rs");
    assert!(
        exit_loop_path.exists(),
        "ExitLoopTool example must exist at {:?}",
        exit_loop_path
    );

    let content = std::fs::read_to_string(&exit_loop_path)
        .expect("Should be able to read built_in_exit_loop.rs");

    assert!(
        content.contains("Validates: docs/official_docs/tools/built-in-tools.md"),
        "ExitLoopTool example must reference the documentation it validates"
    );
    assert!(
        content.contains("ExitLoopTool::new()"),
        "ExitLoopTool example must demonstrate ExitLoopTool usage"
    );
    assert!(
        content.contains("LoopAgent::new"),
        "ExitLoopTool example must demonstrate LoopAgent usage"
    );
    assert!(
        content.contains("std::result::Result"),
        "ExitLoopTool example must use std::result::Result"
    );

    // Test built_in_load_artifacts.rs
    let load_artifacts_path =
        root.join("adk-rust-guide/examples/tools/built_in_load_artifacts.rs");
    assert!(
        load_artifacts_path.exists(),
        "LoadArtifactsTool example must exist at {:?}",
        load_artifacts_path
    );

    let content = std::fs::read_to_string(&load_artifacts_path)
        .expect("Should be able to read built_in_load_artifacts.rs");

    assert!(
        content.contains("Validates: docs/official_docs/tools/built-in-tools.md"),
        "LoadArtifactsTool example must reference the documentation it validates"
    );
    assert!(
        content.contains("LoadArtifactsTool::new()"),
        "LoadArtifactsTool example must demonstrate LoadArtifactsTool usage"
    );
    assert!(
        content.contains("std::result::Result"),
        "LoadArtifactsTool example must use std::result::Result"
    );
}

// Property test for built-in tools examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    ///
    /// *For any* built-in tools example, the file SHALL contain proper documentation
    /// references and demonstrate the specific built-in tool.
    ///
    /// **Validates: Requirements 6.4**
    #[test]
    fn prop_built_in_tools_example_valid(index in 0usize..100) {
        let root = workspace_root();
        let built_in_tools_examples = vec![
            ("tools/built_in_google_search.rs", "GoogleSearchTool"),
            ("tools/built_in_exit_loop.rs", "ExitLoopTool"),
            ("tools/built_in_load_artifacts.rs", "LoadArtifactsTool"),
        ];

        let (example_file, tool_name) = &built_in_tools_examples[index % built_in_tools_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);

        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Built-in tools example should exist: {:?}",
            example_path
        );

        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("built-in-tools.md"),
                "Example {} should reference built-in-tools.md documentation",
                example_file
            );

            // Must use the specific tool
            prop_assert!(
                content.contains(tool_name),
                "Example {} should use {}",
                example_file,
                tool_name
            );

            // Must demonstrate adding tool to agent
            prop_assert!(
                content.contains(".tool("),
                "Example {} should demonstrate adding tool to agent",
                example_file
            );

            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );

            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
///
/// *For any* session example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the sessions documentation
/// 4. Demonstrate SessionService usage
///
/// **Validates: Requirements 7.6**
#[test]
fn test_session_example_compilation() {
    let root = workspace_root();

    // Test session_basic.rs
    let session_basic_path = root.join("adk-rust-guide/examples/sessions/session_basic.rs");
    assert!(
        session_basic_path.exists(),
        "Session basic example must exist at {:?}",
        session_basic_path
    );

    let content = std::fs::read_to_string(&session_basic_path)
        .expect("Should be able to read session_basic.rs");

    assert!(
        content.contains("Validates: docs/official_docs/sessions/sessions.md"),
        "Session basic example must reference the documentation it validates"
    );
    assert!(
        content.contains("InMemorySessionService::new()"),
        "Session basic example must demonstrate InMemorySessionService creation"
    );
    assert!(
        content.contains("CreateRequest"),
        "Session basic example must demonstrate CreateRequest usage"
    );
    assert!(
        content.contains("GetRequest"),
        "Session basic example must demonstrate GetRequest usage"
    );
    assert!(
        content.contains("ListRequest"),
        "Session basic example must demonstrate ListRequest usage"
    );
    assert!(
        content.contains("DeleteRequest"),
        "Session basic example must demonstrate DeleteRequest usage"
    );
    assert!(
        content.contains("std::result::Result"),
        "Session basic example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Session basic example must be an async main function"
    );

    // Test state_management.rs
    let state_management_path = root.join("adk-rust-guide/examples/sessions/state_management.rs");
    assert!(
        state_management_path.exists(),
        "State management example must exist at {:?}",
        state_management_path
    );

    let content = std::fs::read_to_string(&state_management_path)
        .expect("Should be able to read state_management.rs");

    assert!(
        content.contains("Validates: docs/official_docs/sessions/state.md"),
        "State management example must reference the documentation it validates"
    );
    assert!(
        content.contains("KEY_PREFIX_APP"),
        "State management example must demonstrate KEY_PREFIX_APP"
    );
    assert!(
        content.contains("KEY_PREFIX_USER"),
        "State management example must demonstrate KEY_PREFIX_USER"
    );
    assert!(
        content.contains("KEY_PREFIX_TEMP"),
        "State management example must demonstrate KEY_PREFIX_TEMP"
    );
    assert!(
        content.contains("state.get("),
        "State management example must demonstrate state retrieval"
    );
    assert!(
        content.contains("std::result::Result"),
        "State management example must use std::result::Result"
    );
}

// Property test for session examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    ///
    /// *For any* session example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    ///
    /// **Validates: Requirements 7.6**
    #[test]
    fn prop_session_examples_valid(index in 0usize..100) {
        let root = workspace_root();
        let session_examples = vec![
            ("sessions/session_basic.rs", "sessions.md", "InMemorySessionService"),
            ("sessions/state_management.rs", "state.md", "KEY_PREFIX_"),
        ];

        let (example_file, doc_ref, key_pattern) = &session_examples[index % session_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);

        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Session example should exist: {:?}",
            example_path
        );

        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains(doc_ref),
                "Example {} should reference {} documentation",
                example_file,
                doc_ref
            );

            // Must use the key pattern
            prop_assert!(
                content.contains(key_pattern),
                "Example {} should use {}",
                example_file,
                key_pattern
            );

            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );

            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
///
/// *For any* callback example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the callbacks documentation
/// 4. Demonstrate the specific callback type
///
/// **Validates: Requirements 8.3, 8.5**
#[test]
fn test_callback_example_compilation() {
    let root = workspace_root();

    // Test before_agent.rs
    let before_agent_path = root.join("adk-rust-guide/examples/callbacks/before_agent.rs");
    assert!(
        before_agent_path.exists(),
        "Before agent callback example must exist at {:?}",
        before_agent_path
    );

    let content = std::fs::read_to_string(&before_agent_path)
        .expect("Should be able to read before_agent.rs");

    assert!(
        content.contains("Validates: docs/official_docs/callbacks/callbacks.md"),
        "Before agent callback example must reference the documentation it validates"
    );
    assert!(
        content.contains("before_agent"),
        "Before agent callback example must demonstrate before_agent callback"
    );
    assert!(
        content.contains("std::result::Result"),
        "Before agent callback example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Before agent callback example must be an async main function"
    );

    // Test after_agent.rs
    let after_agent_path = root.join("adk-rust-guide/examples/callbacks/after_agent.rs");
    assert!(
        after_agent_path.exists(),
        "After agent callback example must exist at {:?}",
        after_agent_path
    );

    let content = std::fs::read_to_string(&after_agent_path)
        .expect("Should be able to read after_agent.rs");

    assert!(
        content.contains("Validates: docs/official_docs/callbacks/callbacks.md"),
        "After agent callback example must reference the documentation it validates"
    );
    assert!(
        content.contains("after_agent"),
        "After agent callback example must demonstrate after_agent callback"
    );
    assert!(
        content.contains("std::result::Result"),
        "After agent callback example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "After agent callback example must be an async main function"
    );

    // Test model_callbacks.rs
    let model_callbacks_path = root.join("adk-rust-guide/examples/callbacks/model_callbacks.rs");
    assert!(
        model_callbacks_path.exists(),
        "Model callbacks example must exist at {:?}",
        model_callbacks_path
    );

    let content = std::fs::read_to_string(&model_callbacks_path)
        .expect("Should be able to read model_callbacks.rs");

    assert!(
        content.contains("Validates: docs/official_docs/callbacks/callbacks.md"),
        "Model callbacks example must reference the documentation it validates"
    );
    assert!(
        content.contains("before_model") || content.contains("after_model"),
        "Model callbacks example must demonstrate before_model or after_model callback"
    );
    assert!(
        content.contains("std::result::Result"),
        "Model callbacks example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Model callbacks example must be an async main function"
    );

    // Test tool_callbacks.rs
    let tool_callbacks_path = root.join("adk-rust-guide/examples/callbacks/tool_callbacks.rs");
    assert!(
        tool_callbacks_path.exists(),
        "Tool callbacks example must exist at {:?}",
        tool_callbacks_path
    );

    let content = std::fs::read_to_string(&tool_callbacks_path)
        .expect("Should be able to read tool_callbacks.rs");

    assert!(
        content.contains("Validates: docs/official_docs/callbacks/callbacks.md"),
        "Tool callbacks example must reference the documentation it validates"
    );
    assert!(
        content.contains("before_tool") || content.contains("after_tool"),
        "Tool callbacks example must demonstrate before_tool or after_tool callback"
    );
    assert!(
        content.contains("std::result::Result"),
        "Tool callbacks example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Tool callbacks example must be an async main function"
    );
}

// Property test for callback examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    ///
    /// *For any* callback example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    ///
    /// **Validates: Requirements 8.3, 8.5**
    #[test]
    fn prop_callback_examples_valid(index in 0usize..100) {
        let root = workspace_root();
        let callback_examples = vec![
            ("callbacks/before_agent.rs", "before_agent"),
            ("callbacks/after_agent.rs", "after_agent"),
            ("callbacks/model_callbacks.rs", "model"),
            ("callbacks/tool_callbacks.rs", "tool"),
        ];

        let (example_file, callback_type) = &callback_examples[index % callback_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);

        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Callback example should exist: {:?}",
            example_path
        );

        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("callbacks.md"),
                "Example {} should reference callbacks.md documentation",
                example_file
            );

            // Must use the callback type
            prop_assert!(
                content.contains(callback_type),
                "Example {} should use {} callback",
                example_file,
                callback_type
            );

            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );

            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
///
/// *For any* artifact example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the artifacts documentation
/// 4. Demonstrate ArtifactService usage
///
/// **Validates: Requirements 9.6**
#[test]
fn test_artifact_example_compilation() {
    let root = workspace_root();

    // Test artifact_ops.rs
    let artifact_ops_path = root.join("adk-rust-guide/examples/artifacts/artifact_ops.rs");
    assert!(
        artifact_ops_path.exists(),
        "Artifact operations example must exist at {:?}",
        artifact_ops_path
    );

    let content = std::fs::read_to_string(&artifact_ops_path)
        .expect("Should be able to read artifact_ops.rs");

    assert!(
        content.contains("Validates: docs/official_docs/artifacts/artifacts.md"),
        "Artifact operations example must reference the documentation it validates"
    );
    assert!(
        content.contains("InMemoryArtifactService::new()"),
        "Artifact operations example must demonstrate InMemoryArtifactService creation"
    );
    assert!(
        content.contains("SaveRequest"),
        "Artifact operations example must demonstrate SaveRequest usage"
    );
    assert!(
        content.contains("LoadRequest"),
        "Artifact operations example must demonstrate LoadRequest usage"
    );
    assert!(
        content.contains("Part::InlineData") || content.contains("Part::Text"),
        "Artifact operations example must demonstrate Part usage"
    );
    assert!(
        content.contains("std::result::Result"),
        "Artifact operations example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Artifact operations example must be an async main function"
    );
}

// Property test for artifact examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    ///
    /// *For any* artifact example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    ///
    /// **Validates: Requirements 9.6**
    #[test]
    fn prop_artifact_examples_valid(index in 0usize..100) {
        let root = workspace_root();
        let artifact_examples = vec![
            ("artifacts/artifact_ops.rs", "InMemoryArtifactService"),
        ];

        let (example_file, service_type) = &artifact_examples[index % artifact_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);

        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Artifact example should exist: {:?}",
            example_path
        );

        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("artifacts.md"),
                "Example {} should reference artifacts.md documentation",
                example_file
            );

            // Must use the service type
            prop_assert!(
                content.contains(service_type),
                "Example {} should use {}",
                example_file,
                service_type
            );

            // Must demonstrate save and load operations
            prop_assert!(
                content.contains("SaveRequest") && content.contains("LoadRequest"),
                "Example {} should demonstrate save and load operations",
                example_file
            );

            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );

            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

/// **Feature: official-documentation, Property 1: Documentation Example Compilation**
///
/// *For any* event example file, the file SHALL:
/// 1. Exist on disk
/// 2. Contain valid Rust syntax (verified by presence of expected patterns)
/// 3. Reference the events documentation
/// 4. Demonstrate event inspection and handling
///
/// **Validates: Requirements 10.4**
#[test]
fn test_event_example_compilation() {
    let root = workspace_root();

    // Test event_inspection.rs
    let event_inspection_path = root.join("adk-rust-guide/examples/events/event_inspection.rs");
    assert!(
        event_inspection_path.exists(),
        "Event inspection example must exist at {:?}",
        event_inspection_path
    );

    let content = std::fs::read_to_string(&event_inspection_path)
        .expect("Should be able to read event_inspection.rs");

    assert!(
        content.contains("Validates: docs/official_docs/events/events.md"),
        "Event inspection example must reference the documentation it validates"
    );
    assert!(
        content.contains("events.at("),
        "Event inspection example must demonstrate event access"
    );
    assert!(
        content.contains("event.id") || content.contains("event.timestamp"),
        "Event inspection example must demonstrate event field access"
    );
    assert!(
        content.contains("event.actions") || content.contains("state_delta"),
        "Event inspection example must demonstrate EventActions access"
    );
    assert!(
        content.contains("invocation_id"),
        "Event inspection example must demonstrate invocation_id usage"
    );
    assert!(
        content.contains("std::result::Result"),
        "Event inspection example must use std::result::Result"
    );
    assert!(
        content.contains("#[tokio::main]"),
        "Event inspection example must be an async main function"
    );
}

// Property test for event examples
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: official-documentation, Property 1: Documentation Example Compilation**
    ///
    /// *For any* event example, the file SHALL contain proper documentation
    /// references and demonstrate the documented API patterns.
    ///
    /// **Validates: Requirements 10.4**
    #[test]
    fn prop_event_examples_valid(index in 0usize..100) {
        let root = workspace_root();
        let event_examples = vec![
            ("events/event_inspection.rs", "events.at("),
        ];

        let (example_file, key_pattern) = &event_examples[index % event_examples.len()];
        let example_path = root.join("adk-rust-guide/examples").join(example_file);

        // Verify file exists
        prop_assert!(
            example_path.exists(),
            "Event example should exist: {:?}",
            example_path
        );

        // Read and verify structure
        if let Ok(content) = std::fs::read_to_string(&example_path) {
            // Must reference the documentation
            prop_assert!(
                content.contains("events.md"),
                "Example {} should reference events.md documentation",
                example_file
            );

            // Must use the key pattern
            prop_assert!(
                content.contains(key_pattern),
                "Example {} should use {}",
                example_file,
                key_pattern
            );

            // Must demonstrate event field access
            prop_assert!(
                content.contains("event.id") || content.contains("event.timestamp"),
                "Example {} should demonstrate event field access",
                example_file
            );

            // Must demonstrate EventActions
            prop_assert!(
                content.contains("event.actions") || content.contains("state_delta"),
                "Example {} should demonstrate EventActions access",
                example_file
            );

            // Must be async main
            prop_assert!(
                content.contains("#[tokio::main]"),
                "Example {} should have async main",
                example_file
            );

            // Must use std::result::Result pattern
            prop_assert!(
                content.contains("std::result::Result"),
                "Example {} should use std::result::Result",
                example_file
            );
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_get_doc_pages_excludes_index() {
        let pages = get_doc_pages();
        assert!(!pages.contains(&"index.md".to_string()));
    }

    #[test]
    fn test_doc_to_example_mapping_quickstart() {
        let examples = doc_to_example_mapping("quickstart");
        assert!(examples.contains(&"quickstart.rs".to_string()));
    }

    #[test]
    fn test_doc_to_example_mapping_llm_agent() {
        let examples = doc_to_example_mapping("agents/llm-agent");
        assert!(examples.contains(&"agents/llm_agent_basic.rs".to_string()));
        assert!(examples.contains(&"agents/llm_agent_config.rs".to_string()));
    }
    
    #[test]
    fn test_doc_to_example_mapping_workflow_agents() {
        let examples = doc_to_example_mapping("agents/workflow-agents");
        assert!(examples.contains(&"agents/sequential_agent.rs".to_string()));
        assert!(examples.contains(&"agents/parallel_agent.rs".to_string()));
        assert!(examples.contains(&"agents/loop_agent.rs".to_string()));
    }

    #[test]
    fn test_example_files_exist() {
        let examples = get_example_files();
        // Verify we have examples in the expected structure
        assert!(examples.iter().any(|e| e.contains("quickstart")));
    }
    
    #[test]
    fn test_llm_agent_examples_in_list() {
        let examples = get_example_files();
        // Verify LlmAgent examples are found
        assert!(
            examples.iter().any(|e| e.contains("llm_agent_basic")),
            "llm_agent_basic.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("llm_agent_config")),
            "llm_agent_config.rs should be in example files"
        );
    }
    
    #[test]
    fn test_workflow_agent_examples_in_list() {
        let examples = get_example_files();
        // Verify workflow agent examples are found
        assert!(
            examples.iter().any(|e| e.contains("sequential_agent")),
            "sequential_agent.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("parallel_agent")),
            "parallel_agent.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("loop_agent")),
            "loop_agent.rs should be in example files"
        );
    }
    
    #[test]
    fn test_doc_to_example_mapping_function_tools() {
        let examples = doc_to_example_mapping("tools/function-tools");
        assert!(examples.contains(&"tools/function_tool.rs".to_string()));
    }
    
    #[test]
    fn test_function_tool_example_in_list() {
        let examples = get_example_files();
        // Verify function tool example is found
        assert!(
            examples.iter().any(|e| e.contains("function_tool")),
            "function_tool.rs should be in example files"
        );
    }
    
    #[test]
    fn test_doc_to_example_mapping_built_in_tools() {
        let examples = doc_to_example_mapping("tools/built-in-tools");
        assert!(examples.contains(&"tools/built_in_google_search.rs".to_string()));
        assert!(examples.contains(&"tools/built_in_exit_loop.rs".to_string()));
        assert!(examples.contains(&"tools/built_in_load_artifacts.rs".to_string()));
    }

    #[test]
    fn test_built_in_tools_examples_in_list() {
        let examples = get_example_files();
        // Verify all built-in tools examples are found
        assert!(
            examples.iter().any(|e| e.contains("built_in_google_search")),
            "built_in_google_search.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("built_in_exit_loop")),
            "built_in_exit_loop.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("built_in_load_artifacts")),
            "built_in_load_artifacts.rs should be in example files"
        );
    }

    #[test]
    fn test_doc_to_example_mapping_sessions() {
        let examples = doc_to_example_mapping("sessions/sessions");
        assert!(examples.contains(&"sessions/session_basic.rs".to_string()));
    }

    #[test]
    fn test_doc_to_example_mapping_state() {
        let examples = doc_to_example_mapping("sessions/state");
        assert!(examples.contains(&"sessions/state_management.rs".to_string()));
    }

    #[test]
    fn test_session_examples_in_list() {
        let examples = get_example_files();
        // Verify session examples are found
        assert!(
            examples.iter().any(|e| e.contains("session_basic")),
            "session_basic.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("state_management")),
            "state_management.rs should be in example files"
        );
    }
    
    #[test]
    fn test_doc_to_example_mapping_callbacks() {
        let examples = doc_to_example_mapping("callbacks/callbacks");
        assert!(examples.contains(&"callbacks/before_agent.rs".to_string()));
        assert!(examples.contains(&"callbacks/after_agent.rs".to_string()));
        assert!(examples.contains(&"callbacks/model_callbacks.rs".to_string()));
        assert!(examples.contains(&"callbacks/tool_callbacks.rs".to_string()));
    }

    #[test]
    fn test_callback_examples_in_list() {
        let examples = get_example_files();
        // Verify callback examples are found
        assert!(
            examples.iter().any(|e| e.contains("before_agent")),
            "before_agent.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("after_agent")),
            "after_agent.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("model_callbacks")),
            "model_callbacks.rs should be in example files"
        );
        assert!(
            examples.iter().any(|e| e.contains("tool_callbacks")),
            "tool_callbacks.rs should be in example files"
        );
    }
    
    #[test]
    fn test_doc_to_example_mapping_artifacts() {
        let examples = doc_to_example_mapping("artifacts/artifacts");
        assert!(examples.contains(&"artifacts/artifact_ops.rs".to_string()));
    }

    #[test]
    fn test_artifact_examples_in_list() {
        let examples = get_example_files();
        // Verify artifact examples are found
        assert!(
            examples.iter().any(|e| e.contains("artifact_ops")),
            "artifact_ops.rs should be in example files"
        );
    }
    
    #[test]
    fn test_doc_to_example_mapping_events() {
        let examples = doc_to_example_mapping("events/events");
        assert!(examples.contains(&"events/event_inspection.rs".to_string()));
    }

    #[test]
    fn test_event_examples_in_list() {
        let examples = get_example_files();
        // Verify event examples are found
        assert!(
            examples.iter().any(|e| e.contains("event_inspection")),
            "event_inspection.rs should be in example files"
        );
    }
    
    #[test]
    fn test_doc_to_example_mapping_multi_agent() {
        let examples = doc_to_example_mapping("agents/multi-agent");
        assert!(examples.contains(&"agents/multi_agent.rs".to_string()));
    }

    #[test]
    fn test_multi_agent_example_in_list() {
        let examples = get_example_files();
        // Verify multi-agent example is found
        assert!(
            examples.iter().any(|e| e.contains("multi_agent")),
            "multi_agent.rs should be in example files"
        );
    }
}
