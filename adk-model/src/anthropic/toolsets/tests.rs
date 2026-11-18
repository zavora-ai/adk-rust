use super::*;
use crate::anthropic::client::AnthropicClient;
use crate::anthropic::{AnthropicConfig, convert};
use adk_anthropic::{ContentBlock, Message, MessageParamContent, Model, StopReason, Usage};
use adk_core::{Content, FunctionResponseData, InlineDataPart, LlmRequest, Part};
use serde_json::json;

fn member(toolset: &str, member: &str) -> Value {
    json!({
        "name": format!("{toolset}.{member}"),
        "description": "toolset member",
        MEMBER_KEY: { "toolset": toolset, "member": member }
    })
}

fn computer_member(name: &str) -> Value {
    json!({
        "name": format!("computer.{name}"),
        MEMBER_KEY: {
            "toolset": "computer",
            "member": name,
            "display_width_px": 1280,
            "display_height_px": 800
        }
    })
}

fn request(tools: &[(&str, Value)], contents: Vec<Content>) -> LlmRequest {
    let mut request = LlmRequest::new("ignored", contents);
    for (name, declaration) in tools {
        request.tools.insert((*name).to_string(), declaration.clone());
    }
    request
}

fn params(model: &str, request: &LlmRequest) -> adk_anthropic::MessageCreateParams {
    AnthropicClient::build_message_params(model, 1024, request, &AnthropicConfig::default())
        .expect("request builds")
}

fn tools_json(params: &adk_anthropic::MessageCreateParams) -> Value {
    serde_json::to_value(params.tools.as_ref().expect("tools")).unwrap()
}

fn custom(name: &str) -> Value {
    json!({ "name": name, "description": "custom", "parameters": { "type": "object" } })
}

#[test]
fn member_declarations_collapse_into_one_toolset_entry_per_model() {
    let tools = [
        ("lookup", custom("lookup")),
        ("computer.screenshot", computer_member("screenshot")),
        ("computer.left_click", computer_member("left_click")),
        ("computer.type", computer_member("type")),
        ("browser.navigate", member("browser", "navigate")),
        ("browser.read_page", member("browser", "read_page")),
        ("browser.javascript_exec", member("browser", "javascript_exec")),
        ("notes", custom("notes")),
    ];
    let request = request(&tools, vec![Content::new("user").with_text("go")]);

    let current = tools_json(&params("claude-sonnet-5-5", &request));
    let disabled: serde_json::Map<String, Value> = ComputerMember::ALL
        .iter()
        .filter(|member| {
            !matches!(
                member,
                ComputerMember::Screenshot | ComputerMember::LeftClick | ComputerMember::Type
            )
        })
        .map(|member| (member.as_str().to_string(), json!({ "enabled": false })))
        .collect();
    let browser_configs: serde_json::Map<String, Value> = BrowserMember::ALL
        .iter()
        .filter_map(|member| {
            let declared = matches!(
                member,
                BrowserMember::Navigate | BrowserMember::ReadPage | BrowserMember::JavascriptExec
            );
            (declared != member.enabled_by_default())
                .then(|| (member.as_str().to_string(), json!({ "enabled": declared })))
        })
        .collect();
    assert_eq!(current.as_array().unwrap().len(), 4);
    assert_eq!(current[0]["name"], "lookup");
    assert_eq!(current[1], json!({ "type": "computer_toolset_20260801", "configs": disabled }));
    assert_eq!(
        current[2],
        json!({ "type": "browser_toolset_20260801", "configs": browser_configs })
    );
    assert_eq!(current[2]["configs"]["javascript_exec"], json!({ "enabled": true }));
    assert_eq!(current[3]["name"], "notes");
    assert!(params("claude-sonnet-5-5", &request).tool_beta_headers().is_empty());
}

#[test]
fn earlier_models_get_the_computer_version_they_accept() {
    let tools = [
        ("computer.screenshot", computer_member("screenshot")),
        ("computer.zoom", computer_member("zoom")),
    ];
    let request = request(&tools, vec![Content::new("user").with_text("go")]);

    let opus_47 = params("claude-opus-4-7", &request);
    assert_eq!(
        tools_json(&opus_47),
        json!([{
            "type": "computer_20251124",
            "name": "computer",
            "display_width_px": 1280,
            "display_height_px": 800,
            "enable_zoom": true
        }])
    );
    assert_eq!(opus_47.tool_beta_headers(), vec!["computer-use-2025-11-24"]);

    let haiku_45 = params("claude-haiku-4-5-20251001", &request);
    assert_eq!(
        tools_json(&haiku_45),
        json!([{
            "type": "computer_20250124",
            "name": "computer",
            "display_width_px": 1280,
            "display_height_px": 800
        }])
    );
    assert_eq!(haiku_45.tool_beta_headers(), vec!["computer-use-2025-01-24"]);
}

#[test]
fn an_earlier_model_without_a_display_size_fails_with_guidance() {
    let request = request(
        &[("computer.screenshot", member("computer", "screenshot"))],
        vec![Content::new("user").with_text("go")],
    );
    let error = AnthropicClient::build_message_params(
        "claude-sonnet-4-6",
        1024,
        &request,
        &AnthropicConfig::default(),
    )
    .expect_err("the earlier version needs a display size");
    assert!(error.message.contains("display_width_px"), "{}", error.message);
}

#[test]
fn a_declared_earlier_version_is_upgraded_for_a_current_model() {
    let declared = json!({
        "name": "computer",
        "x-adk-anthropic-tool": {
            "type": "computer_20251124",
            "name": "computer",
            "display_width_px": 1024,
            "display_height_px": 768
        }
    });
    let request = request(&[("computer", declared)], vec![Content::new("user").with_text("go")]);
    let upgraded = params("claude-opus-5-5", &request);
    assert_eq!(
        tools_json(&upgraded),
        json!([{ "type": "computer_toolset_20260801", "configs": { "zoom": { "enabled": false } } }])
    );
    assert!(upgraded.tool_beta_headers().is_empty());
    // An identifier that names no Claude model keeps the declared form.
    let kept = params("my-gateway-alias", &request);
    assert_eq!(tools_json(&kept)[0]["type"], "computer_20251124");
}

#[test]
fn the_browser_toolset_is_refused_for_a_model_without_it() {
    let request = request(
        &[("browser.navigate", member("browser", "navigate"))],
        vec![Content::new("user").with_text("go")],
    );
    let error = AnthropicClient::build_message_params(
        "claude-sonnet-4-6",
        1024,
        &request,
        &AnthropicConfig::default(),
    )
    .expect_err("Sonnet 4.6 has no browser toolset");
    assert!(error.message.contains("browser_toolset_20260801"), "{}", error.message);
}

#[test]
fn computer_use_may_be_declared_once() {
    let raw = json!({ "x-adk-anthropic-tool": { "type": "computer_toolset_20260801" } });
    let request = request(
        &[("computer", raw), ("computer.screenshot", member("computer", "screenshot"))],
        vec![Content::new("user").with_text("go")],
    );
    let error = AnthropicClient::build_message_params(
        "claude-sonnet-5-5",
        1024,
        &request,
        &AnthropicConfig::default(),
    )
    .expect_err("two computer use entries");
    assert!(error.message.contains("declared once"), "{}", error.message);
}

fn response(blocks: Vec<ContentBlock>) -> Message {
    let mut message = Message::new(
        "msg".into(),
        blocks,
        Model::Custom("claude-sonnet-5-5".into()),
        Usage::new(1, 1),
    );
    message.stop_reason = Some(StopReason::ToolUse);
    message
}

fn calls(message: &Message, forms: ToolsetForms) -> Vec<(String, Value)> {
    convert::from_anthropic_message(message, forms)
        .0
        .content
        .unwrap()
        .parts
        .into_iter()
        .filter_map(|part| match part {
            Part::FunctionCall { name, args, .. } => Some((name, args)),
            _ => None,
        })
        .collect()
}

#[test]
fn member_calls_become_namespaced_function_calls() {
    let toolset = ToolsetForms { computer: Some(ComputerToolVersion::Toolset20260801) };
    let legacy = ToolsetForms { computer: Some(ComputerToolVersion::Computer20251124) };
    let message = response(vec![
        ContentBlock::ToolUse(ToolUseBlock::member(
            "t1",
            "computer",
            "left_click",
            json!({ "coordinate": [512, 742] }),
        )),
        ContentBlock::ToolUse(ToolUseBlock::member(
            "t2",
            "browser",
            "navigate",
            json!({ "url": "https://example.com" }),
        )),
        ContentBlock::ToolUse(ToolUseBlock::new(
            "t3",
            "computer",
            json!({ "action": "screenshot" }),
        )),
    ]);
    assert_eq!(
        calls(&message, legacy),
        vec![
            ("computer.left_click".to_string(), json!({ "coordinate": [512, 742] })),
            ("browser.navigate".to_string(), json!({ "url": "https://example.com" })),
            ("computer.screenshot".to_string(), json!({})),
        ]
    );
    // Without an earlier computer use version declared, a tool named `computer` is the
    // application's own and keeps its name and arguments.
    assert_eq!(
        calls(&message, toolset)[2],
        ("computer".to_string(), json!({ "action": "screenshot" }))
    );
}

#[test]
fn every_member_round_trips_in_both_forms() {
    for forms in [
        ToolsetForms { computer: Some(ComputerToolVersion::Toolset20260801) },
        ToolsetForms { computer: Some(ComputerToolVersion::Computer20251124) },
        ToolsetForms { computer: Some(ComputerToolVersion::Computer20250124) },
    ] {
        for member in ComputerMember::ALL {
            let name = format!("computer.{member}");
            let args = json!({ "coordinate": [3, 4], "text": "shift" });
            let block = tool_use_from_call("id".into(), &name, &args, forms);
            assert_eq!(block.toolset_name.is_some(), forms.computer.unwrap().is_toolset());
            assert_eq!(call_from_tool_use(&block, forms), (name, args));
        }
        for member in BrowserMember::ALL {
            let name = format!("browser.{member}");
            let args = json!({ "target": { "type": "ref", "ref": "ref_2" } });
            let block = tool_use_from_call("id".into(), &name, &args, forms);
            assert_eq!(block.toolset_name.as_deref(), Some("browser"));
            assert_eq!(call_from_tool_use(&block, forms), (name, args));
        }
    }
}

fn history_blocks(request: &LlmRequest, model: &str) -> Vec<Value> {
    let params = params(model, request);
    params
        .messages
        .iter()
        .flat_map(|message| match &message.content {
            MessageParamContent::Array(blocks) => blocks.clone(),
            MessageParamContent::String(_) => Vec::new(),
        })
        .map(|block| serde_json::to_value(block).unwrap())
        .collect()
}

fn function_call(id: &str, name: &str, args: Value) -> Part {
    Part::FunctionCall { name: name.into(), args, id: Some(id.into()), thought_signature: None }
}

fn function_response(id: &str, response: FunctionResponseData) -> Part {
    Part::FunctionResponse { function_response: response, id: Some(id.into()), annotations: None }
}

#[test]
fn history_echoes_toolset_name_and_carries_browser_state() {
    let png = vec![0x89, b'P', b'N', b'G'];
    let state = json!({
        "tabs": [{ "tab_id": "tab-1", "title": "Docs", "url": "https://example.com/docs", "active": true }],
        "state_changes": [{ "type": "tab_opened", "tab_id": "tab-1" }]
    });
    let contents = vec![
        Content::new("user").with_text("go"),
        Content {
            role: "model".into(),
            parts: vec![
                function_call("t1", "browser.new_tab", json!({})),
                function_call("t2", "browser.screenshot", json!({})),
                function_call("t3", "computer.left_click", json!({ "coordinate": [1, 2] })),
                function_call(
                    "t4",
                    "browser.left_click",
                    json!({ "target": { "type": "ref", "ref": "ref_9" } }),
                ),
            ],
        },
        Content {
            role: "function".into(),
            parts: vec![
                function_response(
                    "t1",
                    FunctionResponseData::new("browser.new_tab", json!({ "browser_state": state })),
                ),
                function_response(
                    "t2",
                    FunctionResponseData::with_multimodal(
                        "browser.screenshot",
                        json!("Screenshot captured."),
                        vec![InlineDataPart {
                            mime_type: "image/png".into(),
                            data: png,
                            uri: None,
                            annotations: None,
                        }],
                        Vec::new(),
                    ),
                ),
                function_response(
                    "t3",
                    FunctionResponseData::new("computer.left_click", json!("OK")),
                ),
                {
                    let mut part = function_response(
                        "t4",
                        FunctionResponseData::new(
                            "browser.left_click",
                            json!({ "text": "Error: ref_9 is stale", "browser_state": state }),
                        ),
                    );
                    part.set_tool_result_status(adk_core::ToolResultStatus::Failed, None);
                    part
                },
            ],
        },
    ];
    let tools = [
        ("browser.new_tab", member("browser", "new_tab")),
        ("browser.screenshot", member("browser", "screenshot")),
        ("browser.left_click", member("browser", "left_click")),
        ("computer.left_click", member("computer", "left_click")),
    ];
    let blocks = history_blocks(&request(&tools, contents), "claude-sonnet-5-5");
    assert_eq!(
        blocks[1],
        json!({ "type": "tool_use", "id": "t1", "name": "new_tab", "toolset_name": "browser", "input": {} })
    );
    assert_eq!(
        blocks[3],
        json!({
            "type": "tool_use",
            "id": "t3",
            "name": "left_click",
            "toolset_name": "computer",
            "input": { "coordinate": [1, 2] }
        })
    );
    // A tab-management result is exactly one browser_state block.
    assert_eq!(
        blocks[5],
        json!({
            "type": "tool_result",
            "tool_use_id": "t1",
            "toolset_name": "browser",
            "content": [{
                "type": "browser_state",
                "tabs": [{ "tab_id": "tab-1", "title": "Docs", "url": "https://example.com/docs", "active": true }],
                "state_changes": [{ "type": "tab_opened", "tab_id": "tab-1" }]
            }]
        })
    );
    assert_eq!(blocks[6]["toolset_name"], "browser");
    assert_eq!(blocks[6]["content"][0], json!({ "type": "text", "text": "Screenshot captured." }));
    assert_eq!(blocks[6]["content"][1]["type"], "image");
    assert_eq!(
        blocks[7],
        json!({ "type": "tool_result", "tool_use_id": "t3", "toolset_name": "computer", "content": "OK" })
    );
    // An error result keeps its text and drops the tab state.
    assert_eq!(
        blocks[8],
        json!({
            "type": "tool_result",
            "tool_use_id": "t4",
            "toolset_name": "browser",
            "is_error": true,
            "content": "Error: ref_9 is stale"
        })
    );
}

#[test]
fn history_takes_the_one_tool_form_for_an_earlier_model() {
    let contents = vec![
        Content::new("user").with_text("go"),
        Content {
            role: "model".into(),
            parts: vec![function_call(
                "t1",
                "computer.left_click",
                json!({ "coordinate": [1, 2] }),
            )],
        },
        Content {
            role: "function".into(),
            parts: vec![function_response(
                "t1",
                FunctionResponseData::new("computer.left_click", json!("OK")),
            )],
        },
    ];
    let request = request(&[("computer.left_click", computer_member("left_click"))], contents);
    let blocks = history_blocks(&request, "claude-opus-4-7");
    assert_eq!(
        blocks[1],
        json!({
            "type": "tool_use",
            "id": "t1",
            "name": "computer",
            "input": { "action": "left_click", "coordinate": [1, 2] }
        })
    );
    assert_eq!(blocks[2], json!({ "type": "tool_result", "tool_use_id": "t1", "content": "OK" }));
}
