//! Two static tools with one name are rejected when the agent is built, not resolved last-wins.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{DUPLICATE_TOOL_NAME_CODE, ErrorCategory, Llm, Tool};
use adk_model::MockLlm;
use adk_tool::FunctionTool;
use serde_json::json;

fn tool(name: &str, answer: &'static str) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(name, "Answers a question.", move |_ctx, _args| async move {
        Ok(json!({ "answer": answer }))
    }))
}

fn model() -> Arc<dyn Llm> {
    Arc::new(MockLlm::new("mock"))
}

#[test]
fn two_tools_with_one_name_fail_the_build() {
    let Err(err) = LlmAgentBuilder::new("support")
        .model(model())
        .tool(tool("lookup", "first"))
        .tool(tool("other", "other"))
        .tool(tool("lookup", "second"))
        .build()
    else {
        panic!("duplicate tool names must fail the build");
    };

    assert_eq!(err.code, DUPLICATE_TOOL_NAME_CODE);
    assert_eq!(err.category, ErrorCategory::InvalidInput);
    assert!(err.message.contains("'support'"), "{}", err.message);
    assert!(err.message.contains("'lookup'"), "{}", err.message);
}

#[test]
fn the_same_tool_registered_twice_fails_the_build() {
    let lookup = tool("lookup", "only");
    let Err(err) =
        LlmAgentBuilder::new("support").model(model()).tool(lookup.clone()).tool(lookup).build()
    else {
        panic!("registering one tool twice must fail the build");
    };
    assert_eq!(err.code, DUPLICATE_TOOL_NAME_CODE);
}

#[test]
fn distinct_tool_names_build() {
    let agent = LlmAgentBuilder::new("support")
        .model(model())
        .tool(tool("lookup", "a"))
        .tool(tool("refund", "b"))
        .build();
    assert!(agent.is_ok(), "{:?}", agent.err());
}
