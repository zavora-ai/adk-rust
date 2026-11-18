//! Live computer use and browser use declarations: each model takes the computer use version
//! it accepts, calls come back as `computer.<member>` and `browser.<member>`, and member
//! results (an image, `browser_state`) are accepted. Require `ANTHROPIC_API_KEY`.
#![cfg(feature = "anthropic")]

use adk_core::{
    Content, FunctionResponseData, GenerateContentConfig, InlineDataPart, Llm, LlmRequest,
    LlmResponse, Part,
};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, Effort};
use futures::TryStreamExt;
use serde_json::{Value, json};

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn member(toolset: &str, member: &str) -> Value {
    json!({
        "name": format!("{toolset}.{member}"),
        "description": format!("{toolset} use member {member}"),
        "x-adk-anthropic-toolset-member": {
            "toolset": toolset,
            "member": member,
            "display_width_px": 1280,
            "display_height_px": 800
        }
    })
}

fn request(prompt: &str, toolset: &str, members: &[&str]) -> LlmRequest {
    let mut request = LlmRequest::new("ignored", vec![Content::new("user").with_text(prompt)]);
    for name in members {
        request.tools.insert(format!("{toolset}.{name}"), member(toolset, name));
    }
    request.config =
        Some(GenerateContentConfig { max_output_tokens: Some(4_096), ..Default::default() });
    request
}

async fn turn(client: &AnthropicClient, request: LlmRequest, stream: bool) -> Content {
    let responses: Vec<LlmResponse> = client
        .generate_content(request, stream)
        .await
        .expect("request accepted")
        .try_collect()
        .await
        .expect("response parsed");
    let last = responses.last().expect("a response");
    assert_eq!(last.error_code, None, "turn failed: {last:?}");
    responses.into_iter().rev().find_map(|response| response.content).expect("turn content")
}

fn calls(content: &Content) -> Vec<(String, String)> {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::FunctionCall { name, id, .. } => Some((name.clone(), id.clone().unwrap())),
            _ => None,
        })
        .collect()
}

fn answer(id: &str, response: FunctionResponseData) -> Part {
    Part::FunctionResponse { function_response: response, id: Some(id.into()), annotations: None }
}

fn screenshot(name: &str) -> FunctionResponseData {
    FunctionResponseData::with_multimodal(
        name,
        json!("Screenshot captured."),
        vec![InlineDataPart {
            mime_type: "image/png".into(),
            data: PNG.to_vec(),
            uri: None,
            annotations: None,
        }],
        Vec::new(),
    )
}

/// Asks for a screenshot, answers every member call (an image for `screenshot`), and
/// returns the calls of the first turn and the content of the second.
async fn computer_round_trip(config: AnthropicConfig, stream: bool) -> (Vec<String>, Content) {
    let client = AnthropicClient::new(config).unwrap();
    let mut request = request(
        "Take one screenshot of the screen and then tell me in one line what you see.",
        "computer",
        &["screenshot", "left_click", "type", "key", "mouse_move", "scroll"],
    );
    let first = turn(&client, request.clone(), stream).await;
    let first_calls = calls(&first);
    assert!(!first_calls.is_empty(), "no member call: {first:?}");
    let results = first_calls
        .iter()
        .map(|(name, id)| {
            let response = if name == "computer.screenshot" {
                screenshot(name)
            } else {
                FunctionResponseData::new(name, json!("OK"))
            };
            answer(id, response)
        })
        .collect();
    request.contents.push(first);
    request.contents.push(Content { role: "function".into(), parts: results });
    let second = turn(&client, request, stream).await;
    (first_calls.into_iter().map(|(name, _)| name).collect(), second)
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_current_models_accept_the_computer_toolset() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else { return };
    for (model, stream) in [("claude-sonnet-5-5", true), ("claude-haiku-5-5", false)] {
        let config = AnthropicConfig::new(key.clone(), model).with_effort(Effort::Low);
        let (first, second) = computer_round_trip(config, stream).await;
        println!("{model}: first turn {first:?}; second turn {:?}", second.parts);
        assert!(first.iter().all(|name| name.starts_with("computer.")), "{model}: {first:?}");
        assert!(first.contains(&"computer.screenshot".to_string()), "{model}: {first:?}");
    }
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_earlier_model_keeps_its_computer_version() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else { return };
    let config = AnthropicConfig::new(key, "claude-sonnet-4-6").with_effort(Effort::Low);
    let (first, second) = computer_round_trip(config, false).await;
    println!("claude-sonnet-4-6: first turn {first:?}; second turn {:?}", second.parts);
    assert!(first.contains(&"computer.screenshot".to_string()), "{first:?}");
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_browser_toolset_round_trip_with_browser_state() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else { return };
    let client = AnthropicClient::new(
        AnthropicConfig::new(key, "claude-haiku-5-5").with_effort(Effort::Low),
    )
    .unwrap();
    let mut request = request(
        "Open https://example.com in the browser, read the page, and tell me its heading.",
        "browser",
        &[
            "navigate",
            "read_page",
            "get_page_text",
            "screenshot",
            "left_click",
            "new_tab",
            "list_tabs",
        ],
    );
    let first = turn(&client, request.clone(), true).await;
    let first_calls = calls(&first);
    println!("first turn: {first_calls:?}");
    assert_eq!(first_calls.first().map(|(name, _)| name.as_str()), Some("browser.navigate"));
    let state = json!({
        "tabs": [{
            "tab_id": "tab-1",
            "title": "Example Domain",
            "url": "https://example.com/",
            "active": true
        }]
    });
    let results = first_calls
        .iter()
        .map(|(name, id)| {
            let response = match name.as_str() {
                "browser.navigate" => FunctionResponseData::new(
                    name,
                    json!({ "text": "Navigated to https://example.com/", "browser_state": state }),
                ),
                "browser.screenshot" => screenshot(name),
                _ => FunctionResponseData::new(
                    name,
                    json!("heading \"Example Domain\" [ref_1]\nlink \"Learn more\" [ref_2]"),
                ),
            };
            answer(id, response)
        })
        .collect();
    request.contents.push(first);
    request.contents.push(Content { role: "function".into(), parts: results });
    let second = turn(&client, request, true).await;
    println!("second turn: {:?}", second.parts);
    assert!(
        second.parts.iter().any(
            |part| matches!(part, Part::FunctionCall { name, .. } if name.starts_with("browser."))
                || part.text().is_some_and(|text| !text.trim().is_empty())
        ),
        "{second:?}"
    );
}
