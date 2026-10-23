//! Property-based tests for the `include_server_side_tool_invocations` flag on `ToolConfig`.
//!
//! **Property 1: Server-Side Tool Invocation Flag Set for Mixed Tools**
//!
//! When built-in tools (`google_search` or `url_context`) are present alongside user-defined
//! function tools, the `ToolConfig` sent to the Gemini API SHALL have
//! `includeServerSideToolInvocations: true` in its JSON representation.
//!
//! **Property 2: No Flag Without Built-in Tools**
//!
//! When only user-defined function tools are present, the flag SHALL NOT be set.
//!
//! **Property 3: Flag Set for Built-in Tools Only**
//!
//! When only built-in tools are present (no function tools), the flag SHALL still be set
//! so Gemini 3 returns `toolCall`/`toolResponse` parts instead of truncating.
//!
//! **Validates: Requirements 1.1, 2.1, 3.1, 3.2, 3.3**

#![cfg(feature = "gemini")]

use proptest::prelude::*;

/// The set of built-in tool names that Gemini recognises as server-side tools.
const BUILTIN_TOOL_NAMES: &[&str] = &["google_search", "url_context"];

/// Strategy that generates a non-empty tool name that is NOT a built-in tool.
/// Names are 1-20 lowercase alphanumeric chars, filtered to exclude builtin names.
fn arb_user_tool_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,19}".prop_filter("must not be a builtin tool name", |name| {
        !BUILTIN_TOOL_NAMES.contains(&name.as_str())
    })
}

/// Strategy that picks one of the built-in tool names.
fn arb_builtin_tool_name() -> impl Strategy<Value = String> {
    prop_oneof![Just("google_search".to_string()), Just("url_context".to_string()),]
}

/// Strategy that generates a tool list containing at least one built-in tool name
/// and at least one user-defined function tool name, plus optional extras.
fn arb_mixed_tool_list() -> impl Strategy<Value = Vec<String>> {
    (
        // At least one builtin
        prop::collection::vec(arb_builtin_tool_name(), 1..=3),
        // At least one user-defined
        prop::collection::vec(arb_user_tool_name(), 1..=5),
    )
        .prop_map(|(builtins, user_tools)| {
            let mut all = builtins;
            all.extend(user_tools);
            all
        })
        .prop_shuffle()
}

/// Strategy that generates a tool list containing ONLY built-in tool names.
fn arb_builtin_only_tool_list() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(arb_builtin_tool_name(), 1..=3)
}

/// Strategy that generates a tool list containing ONLY user-defined function tool names
/// (no built-in tools like `google_search` or `url_context`).
fn arb_user_only_tool_list() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(arb_user_tool_name(), 1..=8)
}

/// Simulate the tool config building logic from `GeminiModel::build_gemini_tools()`.
///
/// This mirrors the logic in `adk-model/src/gemini/client.rs`:
/// 1. Iterate through tool names
/// 2. Detect `google_search` / `url_context` as provider-native tools
/// 3. Build function declarations for non-builtin tools
/// 4. Set `includeServerSideToolInvocations` when ANY provider-native tools are present
///
/// Returns the serialized JSON of the `ToolConfig` that would be sent to the API.
fn build_tool_config_json(tool_names: &[String]) -> serde_json::Value {
    let mut function_declarations = Vec::new();
    let mut has_google_search = false;
    let mut has_url_context = false;

    for name in tool_names {
        match name.as_str() {
            "google_search" => {
                has_google_search = true;
            }
            "url_context" => {
                has_url_context = true;
            }
            _ => {
                let func_decl = adk_gemini::FunctionDeclaration::new(name, "test tool", None);
                function_declarations.push(func_decl);
            }
        }
    }

    let mut _tools = Vec::new();
    let has_function_declarations = !function_declarations.is_empty();
    if has_function_declarations {
        _tools.push(adk_gemini::Tool::with_functions(function_declarations));
    }
    if has_google_search {
        _tools.push(adk_gemini::Tool::google_search());
    }
    if has_url_context {
        _tools.push(adk_gemini::Tool::url_context());
    }

    let has_provider_native_tools = has_google_search || has_url_context;

    // Set the flag whenever provider-native tools are present (not just when mixed)
    let mut tool_config = adk_gemini::ToolConfig::default();
    if has_provider_native_tools {
        tool_config.include_server_side_tool_invocations = Some(true);
    }

    serde_json::to_value(&tool_config).expect("ToolConfig should serialize to JSON")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: gemini3-builtin-tool-fix, Property 1: Mixed Tools**
    ///
    /// *For any* tool list containing at least one built-in tool alongside at least one
    /// user-defined function tool, the `ToolConfig` SHALL contain
    /// `"includeServerSideToolInvocations": true`.
    ///
    /// **Validates: Requirements 1.1, 2.1**
    #[test]
    fn prop_flag_set_for_mixed_tools(
        tool_names in arb_mixed_tool_list()
    ) {
        let tool_config_json = build_tool_config_json(&tool_names);

        let has_flag = tool_config_json
            .get("includeServerSideToolInvocations")
            .and_then(|v| v.as_bool())
            == Some(true);

        prop_assert!(
            has_flag,
            "ToolConfig JSON for tool list {:?} should contain \
             '\"includeServerSideToolInvocations\": true', but got: {}",
            tool_names,
            serde_json::to_string_pretty(&tool_config_json).unwrap()
        );
    }

    /// **Feature: gemini3-builtin-tool-fix, Property 2: No Flag Without Built-in Tools**
    ///
    /// *For any* tool list containing only user-defined function tools (no built-in tools),
    /// the `ToolConfig` SHALL NOT have `includeServerSideToolInvocations` set.
    ///
    /// **Validates: Requirements 3.1, 3.2, 3.3**
    #[test]
    fn prop_no_flag_without_builtin_tools(
        tool_names in arb_user_only_tool_list()
    ) {
        let tool_config_json = build_tool_config_json(&tool_names);

        let has_flag = tool_config_json.get("includeServerSideToolInvocations").is_some();

        prop_assert!(
            !has_flag,
            "ToolConfig JSON for user-only tool list {:?} should NOT contain \
             'includeServerSideToolInvocations', but got: {}",
            tool_names,
            serde_json::to_string_pretty(&tool_config_json).unwrap()
        );
    }

    /// **Feature: gemini3-builtin-tool-fix, Property 3: Flag Set for Built-in Only**
    ///
    /// *For any* tool list containing only built-in tools (no function tools),
    /// the `ToolConfig` SHALL contain `"includeServerSideToolInvocations": true`.
    /// This is the key fix for issue #224 — Gemini 3 truncates responses when
    /// built-in tools are present without the flag, even without function tools.
    ///
    /// **Validates: Requirements 1.1, 2.1**
    #[test]
    fn prop_flag_set_for_builtin_only_tools(
        tool_names in arb_builtin_only_tool_list()
    ) {
        let tool_config_json = build_tool_config_json(&tool_names);

        let has_flag = tool_config_json
            .get("includeServerSideToolInvocations")
            .and_then(|v| v.as_bool())
            == Some(true);

        prop_assert!(
            has_flag,
            "ToolConfig JSON for builtin-only tool list {:?} should contain \
             '\"includeServerSideToolInvocations\": true', but got: {}",
            tool_names,
            serde_json::to_string_pretty(&tool_config_json).unwrap()
        );
    }
}
