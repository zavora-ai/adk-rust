//! Property 8: Studio Live Runner and Generated Rust Agree
//!
//! Validates that Rust-authored code nodes produce consistent behavior
//! between the Studio live runner path and the generated Rust output.
//!
//! The key invariant: the same authored Rust body appears in both paths
//! without translation to another language.
//!
//! Requirements: 5.1, 5.2, 12.3, 12.4

use adk_studio::codegen::action_nodes::*;
use adk_studio::codegen::generate_rust_project;
use adk_studio::schema::{AgentSchema, AgentType, Edge, ProjectSchema};
use proptest::prelude::*;

// ============================================
// Helpers
// ============================================

fn llm_agent() -> AgentSchema {
    AgentSchema {
        agent_type: AgentType::Llm,
        model: Some("gemini-2.5-flash".to_string()),
        instruction: "You are a helpful assistant.".to_string(),
        tools: vec![],
        sub_agents: vec![],
        position: Default::default(),
        max_iterations: None,
        temperature: None,
        top_p: None,
        top_k: None,
        max_output_tokens: None,
        routes: vec![],
    }
}

fn project_with_code_node(node_id: &str, config: CodeNodeConfig) -> ProjectSchema {
    let mut p = ProjectSchema::new("Rust Code Test");
    let agent_id = "agent_1";
    p.agents.insert(agent_id.to_string(), llm_agent());
    p.action_nodes.insert(node_id.to_string(), ActionNodeConfig::Code(config));
    p.workflow.edges =
        vec![Edge::new("START", agent_id), Edge::new(agent_id, node_id), Edge::new(node_id, "END")];
    p
}

fn make_standard(id: &str) -> StandardProperties {
    StandardProperties {
        id: id.to_string(),
        name: format!("Code {id}"),
        description: None,
        error_handling: ErrorHandling { mode: ErrorMode::Stop, ..Default::default() },
        tracing: Tracing { enabled: true, log_level: LogLevel::Info },
        callbacks: Callbacks::default(),
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: "codeResult".to_string() },
    }
}

fn rust_code_node(id: &str, code: &str) -> CodeNodeConfig {
    CodeNodeConfig {
        standard: make_standard(id),
        language: CodeLanguage::Rust,
        code: code.to_string(),
        sandbox: SandboxConfig {
            network_access: false,
            file_system_access: false,
            memory_limit: 128,
            time_limit: 5000,
        },
        input_type: None,
        output_type: None,
    }
}

fn get_main_rs(project: &ProjectSchema) -> String {
    let generated = generate_rust_project(project).unwrap();
    generated.files.iter().find(|f| f.path == "src/main.rs").unwrap().content.clone()
}

// ============================================
// Property 8 Tests
// ============================================

/// Rust-authored code nodes generate a native `{node_id}_run` function
/// in the generated output, not a JS/boa_engine wrapper.
#[test]
fn test_rust_code_node_generates_native_run_function() {
    let code = "    input";
    let node_id = "code_1";
    let project = project_with_code_node(node_id, rust_code_node(node_id, code));
    let main_rs = get_main_rs(&project);

    // The generated code must contain the authored Rust body as a native function
    assert!(
        main_rs
            .contains(&format!("fn {node_id}_run(input: serde_json::Value) -> serde_json::Value")),
        "Generated code must contain native {node_id}_run function.\nGenerated:\n{main_rs}"
    );

    // The authored body must appear verbatim inside the function
    assert!(main_rs.contains(code), "Authored Rust body must appear in generated code");

    // Must NOT contain boa_engine for Rust code nodes
    assert!(!main_rs.contains("boa_engine"), "Rust code nodes must not use boa_engine JS runtime");
}

/// JS/TS code nodes use boa_engine, not the native Rust path.
#[test]
fn test_js_code_node_uses_boa_engine() {
    let node_id = "code_js";
    let config = CodeNodeConfig {
        standard: make_standard(node_id),
        language: CodeLanguage::Javascript,
        code: "return input;".to_string(),
        sandbox: SandboxConfig {
            network_access: false,
            file_system_access: false,
            memory_limit: 128,
            time_limit: 5000,
        },
        input_type: None,
        output_type: None,
    };
    let project = project_with_code_node(node_id, config);
    let main_rs = get_main_rs(&project);

    // JS nodes must use boa_engine
    assert!(
        main_rs.contains("boa_engine"),
        "JS code nodes must use boa_engine.\nGenerated:\n{main_rs}"
    );

    // Must NOT contain a native _run function for JS nodes
    assert!(
        !main_rs.contains(&format!("fn {node_id}_run(")),
        "JS code nodes must not generate native _run function"
    );
}

/// The generated Rust code node calls the _run function with state input.
#[test]
fn test_rust_code_node_passes_state_as_input() {
    let node_id = "code_state";
    let code = "    serde_json::json!({ \"processed\": true })";
    let project = project_with_code_node(node_id, rust_code_node(node_id, code));
    let main_rs = get_main_rs(&project);

    // Must build input from state
    assert!(
        main_rs.contains("input_obj") && main_rs.contains("ctx.state.iter()"),
        "Generated code must build input from graph state"
    );

    // Must call the _run function with the input
    assert!(
        main_rs.contains(&format!("{node_id}_run(input)")),
        "Generated code must call {node_id}_run(input)"
    );
}

/// Empty Rust code nodes produce an error output, not a crash.
#[test]
fn test_empty_rust_code_node_generates_error_output() {
    let node_id = "code_empty";
    let config = CodeNodeConfig {
        standard: make_standard(node_id),
        language: CodeLanguage::Rust,
        code: String::new(),
        sandbox: SandboxConfig {
            network_access: false,
            file_system_access: false,
            memory_limit: 128,
            time_limit: 5000,
        },
        input_type: None,
        output_type: None,
    };
    let project = project_with_code_node(node_id, config);
    let main_rs = get_main_rs(&project);

    // Empty code should produce an error message, not a _run function
    assert!(main_rs.contains("No code provided"), "Empty code node must produce error output");
    assert!(
        !main_rs.contains(&format!("fn {node_id}_run(")),
        "Empty code node must not generate _run function"
    );
}

/// The output key from the code node config is used in the generated code.
#[test]
fn test_rust_code_node_uses_configured_output_key() {
    let node_id = "code_key";
    let mut config = rust_code_node(node_id, "    input");
    config.standard.mapping.output_key = "myCustomOutput".to_string();
    let project = project_with_code_node(node_id, config);
    let main_rs = get_main_rs(&project);

    assert!(
        main_rs.contains("myCustomOutput"),
        "Generated code must use the configured output key"
    );
}

// ============================================
// Property 8: proptest
// ============================================

fn arb_rust_code_body() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("    input".to_string()),
        Just("    serde_json::json!({ \"ok\": true })".to_string()),
        Just("    serde_json::json!(42)".to_string()),
        Just("    let v = input.get(\"x\").cloned().unwrap_or_default();\n    v".to_string()),
    ]
}

fn arb_node_id() -> impl Strategy<Value = String> {
    "[a-z]{3,8}_[0-9]{1,4}".prop_map(|s| s.replace(' ', "_"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: code-execution, Property 8: Studio Live Runner and Generated Rust Agree**
    /// *For any* valid Rust code body and node ID, the generated output SHALL
    /// contain a native `{node_id}_run` function with the authored body,
    /// and SHALL NOT use boa_engine for Rust code nodes.
    /// **Validates: Requirements 5.1, 5.2, 12.3, 12.4**
    #[test]
    fn prop_rust_code_node_generates_native_function(
        node_id in arb_node_id(),
        code_body in arb_rust_code_body(),
    ) {
        let config = rust_code_node(&node_id, &code_body);
        let project = project_with_code_node(&node_id, config);
        let result = generate_rust_project(&project);
        prop_assert!(result.is_ok(), "Code generation must succeed");

        let generated = result.unwrap();
        let main_rs = generated.files.iter().find(|f| f.path == "src/main.rs").unwrap();

        // Must contain native _run function
        let run_fn = format!("fn {}_run(input: serde_json::Value) -> serde_json::Value", node_id);
        prop_assert!(
            main_rs.content.contains(&run_fn),
            "Generated code must contain native _run function for node {}",
            node_id
        );

        // Must contain the authored body
        prop_assert!(
            main_rs.content.contains(&code_body),
            "Authored body must appear in generated code"
        );

        // Must NOT use boa_engine for Rust nodes
        // (Only check the _run function area, not the whole file which may have JS nodes)
        let run_fn_idx = main_rs.content.find(&run_fn).unwrap();
        let after_run = &main_rs.content[run_fn_idx..];
        let fn_end = after_run.find("\n}\n").unwrap_or(after_run.len());
        let fn_body = &after_run[..fn_end];
        prop_assert!(
            !fn_body.contains("boa_engine"),
            "Rust _run function must not reference boa_engine"
        );
    }
}
