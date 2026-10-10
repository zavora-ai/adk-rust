use super::*;
use crate::anthropic::convert;
use adk_anthropic::{Model, StopReason, TextBlock, ToolUseBlock, Usage};

fn message() -> Message {
    Message::new(
        "fixture".into(),
        vec![
            ContentBlock::Text(TextBlock::with_citations(
                "来源🙂",
                vec![TextCitation::web_search_result_location(
                    "source".into(),
                    "encrypted-index".into(),
                    "https://example.com".into(),
                    Some("Source".into()),
                )],
            )),
            ContentBlock::ToolUse(ToolUseBlock::new(
                "read",
                "read_file",
                json!({"path":"before.txt"}),
            )),
        ],
        Model::Custom("fixture".into()),
        Usage::new(8, 4),
    )
}

#[test]
fn preserves_native_blocks_and_unicode_citations() {
    let original = message();
    let response = convert::from_anthropic_message(&original).0;
    let metadata = response.citation_metadata.unwrap();
    assert_eq!(metadata.citation_sources[0].end_index, Some(3));
    let restored = restore(response.content.as_ref().unwrap()).unwrap();
    assert_eq!(restored, original.content);
}

#[test]
fn respects_removed_calls_and_changed_arguments() {
    let mut content = convert::from_anthropic_message(&message()).0.content.unwrap();
    let Part::FunctionCall { args, .. } = &mut content.parts[1] else { panic!("call expected") };
    *args = json!({"path":"after.txt"});
    let blocks = restore(&content).unwrap();
    assert!(matches!(&blocks[1], ContentBlock::ToolUse(tool) if tool.input["path"] == "after.txt"));
    content.parts.remove(1);
    assert!(
        restore(&content).unwrap().iter().all(|block| !matches!(block, ContentBlock::ToolUse(_)))
    );
}

#[test]
fn edited_text_falls_back_to_plain_conversion() {
    let mut content = convert::from_anthropic_message(&message()).0.content.unwrap();
    // A guardrail that redacts the answer after the response, for example.
    content.parts[0] = Part::Text { text: "[REDACTED]".into() };
    assert_eq!(restore(&content), None);

    let request = convert::content_to_message(&content, false).unwrap();
    assert_eq!(
        request.content,
        adk_anthropic::MessageParamContent::Array(vec![
            ContentBlock::Text(TextBlock::new("[REDACTED]".to_string())),
            ContentBlock::ToolUse(ToolUseBlock::new(
                "read",
                "read_file",
                json!({"path":"before.txt"}),
            )),
        ])
    );
}

#[test]
fn malformed_carriers_fall_back_to_plain_conversion() {
    let mut content = convert::from_anthropic_message(&message()).0.content.unwrap();
    content.parts = vec![Part::ServerToolResponse {
        server_tool_response: json!({"type":KIND,"content":"invalid"}),
    }];
    assert_eq!(restore(&content), None);
    content.role = "user".into();
    content.parts = convert::from_anthropic_message(&message()).0.content.unwrap().parts;
    assert_eq!(restore(&content), None);
}

#[test]
fn marks_a_paused_turn_for_continuation() {
    let mut message = message();
    message.stop_reason = Some(StopReason::PauseTurn);
    let response = convert::from_anthropic_message(&message).0;
    assert!(!response.turn_complete);
    assert_eq!(response.provider_metadata.unwrap()["continue_turn"], true);
}

#[test]
fn preserves_refusal_and_context_limit_reasons() {
    for (stop, reason) in [
        (StopReason::Refusal, adk_core::FinishReason::Safety),
        (StopReason::ModelContextWindowExceeded, adk_core::FinishReason::MaxTokens),
    ] {
        let mut message = message();
        message.stop_reason = Some(stop);
        assert_eq!(convert::from_anthropic_message(&message).0.finish_reason, Some(reason));
    }
}

/// A tool-use turn from a model with `display: "omitted"`: an empty reasoning block,
/// a summarized one, a redacted one, and the call they lead to.
fn thinking_tool_turn(redacted: bool) -> Message {
    let mut content = vec![
        ContentBlock::Thinking(adk_anthropic::ThinkingBlock::new("", "EosnCkYICxIMMb3LzNrMu")),
        ContentBlock::Thinking(adk_anthropic::ThinkingBlock::new("Need the weather.", "Es8CCkYI")),
    ];
    if redacted {
        content.push(ContentBlock::RedactedThinking(adk_anthropic::RedactedThinkingBlock::new(
            "EmwKAhgBEgy3va3pzix",
        )));
    }
    content.push(ContentBlock::ToolUse(ToolUseBlock::new(
        "toolu_1",
        "get_weather",
        json!({"city": "Paris"}),
    )));
    let mut message = Message::new(
        "msg_thinking".into(),
        content,
        Model::Custom("fixture".into()),
        Usage::new(8, 4),
    );
    message.stop_reason = Some(StopReason::ToolUse);
    message
}

fn replayed_blocks(content: &Content) -> Vec<ContentBlock> {
    match convert::content_to_message(content, false).unwrap().content {
        adk_anthropic::MessageParamContent::Array(blocks) => blocks,
        other => panic!("expected content blocks, got {other:?}"),
    }
}

#[test]
fn signed_thinking_blocks_replay_unchanged_in_a_tool_use_turn() {
    let original = thinking_tool_turn(false);
    let content = convert::from_anthropic_message(&original).0.content.unwrap();
    assert_eq!(replayed_blocks(&content), original.content);
}

#[test]
fn redacted_thinking_replays_unchanged_in_a_tool_use_turn() {
    let original = thinking_tool_turn(true);
    let content = convert::from_anthropic_message(&original).0.content.unwrap();
    assert_eq!(replayed_blocks(&content), original.content);
}

#[test]
fn streamed_thinking_keeps_its_signature_for_replay() {
    let events = [
        json!({"type":"message_start","message":{"id":"msg_stream","type":"message","role":"assistant","content":[],"model":"fixture","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":8,"output_tokens":1}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EosnCkYICxIMMb3LzNrMu"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Paris\"}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":20}}),
        json!({"type":"message_stop"}),
    ]
    .map(|event| Ok(serde_json::from_value::<adk_anthropic::MessageStreamEvent>(event).unwrap()));
    let responses = futures::executor::block_on(futures::TryStreamExt::try_collect::<Vec<_>>(
        super::super::streaming::responses(futures::stream::iter(events)),
    ))
    .unwrap();
    let complete = responses.last().unwrap().content.clone().unwrap();

    assert_eq!(
        replayed_blocks(&complete),
        vec![
            ContentBlock::Thinking(adk_anthropic::ThinkingBlock::new("", "EosnCkYICxIMMb3LzNrMu")),
            ContentBlock::ToolUse(ToolUseBlock::new(
                "toolu_1",
                "get_weather",
                json!({"city": "Paris"})
            )),
        ]
    );
}

#[test]
fn unsigned_thinking_is_not_replayed_as_assistant_text() {
    let content = Content {
        role: "model".into(),
        parts: vec![
            Part::Thinking { thinking: "another provider's reasoning".into(), signature: None },
            Part::Text { text: "Answer.".into() },
        ],
    };
    assert_eq!(replayed_blocks(&content), vec![ContentBlock::Text(TextBlock::new("Answer."))]);
}

/// The web fetch turn shown in the web fetch tool documentation, as the API sends it.
fn web_fetch_turn_json() -> serde_json::Value {
    json!([
        {"type": "text", "text": "I'll fetch the content from the article to analyze it."},
        {
            "type": "server_tool_use",
            "id": "srvtoolu_01234567890abcdef",
            "name": "web_fetch",
            "input": {"url": "https://example.com/article"}
        },
        {
            "type": "web_fetch_tool_result",
            "tool_use_id": "srvtoolu_01234567890abcdef",
            "content": {
                "type": "web_fetch_result",
                "url": "https://example.com/article",
                "content": {
                    "type": "document",
                    "source": {"type": "text", "media_type": "text/plain", "data": "Full text..."},
                    "title": "Article Title",
                    "citations": {"enabled": true}
                },
                "retrieved_at": "2025-08-25T10:30:00Z"
            }
        },
        {"type": "server_tool_use", "id": "srvtoolu_a93jad", "name": "web_fetch",
         "input": {"url": "https://example.com/missing"}},
        {
            "type": "web_fetch_tool_result",
            "tool_use_id": "srvtoolu_a93jad",
            "content": {"type": "web_fetch_tool_result_error", "error_code": "url_not_accessible"}
        },
        {"type": "text", "text": "Based on the article, AI will transform healthcare."}
    ])
}

fn web_fetch_turn() -> Message {
    let content: Vec<ContentBlock> = serde_json::from_value(web_fetch_turn_json()).unwrap();
    let mut message = Message::new(
        "msg_fetch".into(),
        content,
        Model::Custom("fixture".into()),
        Usage::new(8, 4),
    );
    message.stop_reason = Some(StopReason::EndTurn);
    message
}

fn wire(blocks: &[ContentBlock]) -> serde_json::Value {
    serde_json::to_value(blocks).unwrap()
}

#[test]
fn web_fetch_results_replay_as_received() {
    let content = convert::from_anthropic_message(&web_fetch_turn()).0.content.unwrap();
    assert_eq!(wire(&replayed_blocks(&content)), web_fetch_turn_json());
}

#[test]
fn web_fetch_results_survive_the_adk_part_fallback() {
    let mut content = convert::from_anthropic_message(&web_fetch_turn()).0.content.unwrap();
    // Drop the native copy so the request is built from the ADK parts alone.
    content.parts.retain(|part| {
        !matches!(part, Part::ServerToolResponse { server_tool_response }
            if server_tool_response["type"] == KIND)
    });
    assert_eq!(wire(&replayed_blocks(&content)), web_fetch_turn_json());
}
