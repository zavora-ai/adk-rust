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
    let restored = restore(response.content.as_ref().unwrap()).unwrap().unwrap();
    assert_eq!(restored, original.content);
}

#[test]
fn respects_removed_calls_and_changed_arguments() {
    let mut content = convert::from_anthropic_message(&message()).0.content.unwrap();
    let Part::FunctionCall { args, .. } = &mut content.parts[1] else { panic!("call expected") };
    *args = json!({"path":"after.txt"});
    let blocks = restore(&content).unwrap().unwrap();
    assert!(matches!(&blocks[1], ContentBlock::ToolUse(tool) if tool.input["path"] == "after.txt"));
    content.parts.remove(1);
    assert!(
        restore(&content)
            .unwrap()
            .unwrap()
            .iter()
            .all(|block| !matches!(block, ContentBlock::ToolUse(_)))
    );
}

#[test]
fn rejects_changed_text_and_malformed_carriers() {
    let mut content = convert::from_anthropic_message(&message()).0.content.unwrap();
    content.parts[0] = Part::Text { text: "Changed".into() };
    assert!(restore(&content).is_err());
    content.parts = vec![Part::ServerToolResponse {
        server_tool_response: json!({"type":KIND,"content":"invalid"}),
    }];
    assert!(restore(&content).is_err());
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
