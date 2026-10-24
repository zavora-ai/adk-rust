use super::*;
use adk_core::{Content, Part};

fn response(parts: Vec<Part>, partial: bool) -> adk_core::Event {
    let mut event = adk_core::Event::with_id("response", "invocation");
    event.llm_response.partial = partial;
    event.llm_response.content = Some(Content { role: "model".into(), parts });
    if !partial {
        event.llm_response.provider_metadata = Some(json!({"content_complete":true}));
    }
    event
}

#[test]
fn snapshots_close_original_messages_and_preserve_whitespace() {
    for suffix in ["", " "] {
        let mut stream = AgUiStream::default();
        let thinking =
            response(vec![Part::Thinking { thinking: "Thought".into(), signature: None }], true);
        let text = response(vec![Part::Text { text: "Hello".into() }], true);
        let mut output = stream.translate(&thinking, "thread", "run");
        output.extend(stream.translate(&text, "thread", "run"));
        let terminal = response(
            vec![
                Part::Thinking { thinking: "Thought".into(), signature: Some("signed".into()) },
                Part::Text { text: format!("Hello{suffix}") },
            ],
            false,
        );
        output.extend(stream.translate(&terminal, "thread", "run"));
        let deltas: String = output
            .iter()
            .filter(|item| {
                matches!(item["type"].as_str(), Some("TEXT_MESSAGE_CHUNK" | "TEXT_MESSAGE_CONTENT"))
            })
            .filter_map(|item| item["delta"].as_str())
            .collect();
        assert_eq!(deltas, format!("Hello{suffix}"));
        for (kind, id) in [
            ("TEXT_MESSAGE_END", "response-text-0"),
            ("REASONING_MESSAGE_END", "response-reasoning-0"),
        ] {
            let endings: Vec<_> = output.iter().filter(|item| item["type"] == kind).collect();
            assert_eq!(endings.len(), 1);
            assert_eq!(endings[0]["messageId"], id);
        }
        assert!(!output.iter().any(|item| matches!(
            item["type"].as_str(),
            Some("TEXT_MESSAGE_START" | "REASONING_MESSAGE_START" | "REASONING_MESSAGE_CONTENT")
        )));
        assert!(stream.messages.is_empty());
    }
}

#[test]
fn unstreamed_response_keeps_start_content_and_end() {
    let mut stream = AgUiStream::default();
    let event = response(vec![Part::Text { text: "Hello".into() }], false);
    let output = stream.translate(&event, "thread", "run");
    let kinds: Vec<_> = output.iter().map(|item| item["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["TEXT_MESSAGE_START", "TEXT_MESSAGE_CONTENT", "TEXT_MESSAGE_END"]);
}

#[test]
fn closes_messages_omitted_from_the_terminal_snapshot() {
    for keep_text in [false, true] {
        let mut stream = AgUiStream::default();
        let partial = response(
            vec![
                Part::Thinking { thinking: "Draft thought".into(), signature: None },
                Part::Text { text: "Hello".into() },
            ],
            true,
        );
        stream.translate(&partial, "thread", "run");
        let mut terminal = response(vec![Part::Text { text: "Hello".into() }], false);
        if !keep_text {
            terminal.llm_response.content = None;
        }
        let output = stream.translate(&terminal, "thread", "run");
        for (kind, id) in [
            ("TEXT_MESSAGE_END", "response-text-1"),
            ("REASONING_MESSAGE_END", "response-reasoning-0"),
        ] {
            assert_eq!(
                output
                    .iter()
                    .filter(|item| item["type"] == kind && item["messageId"] == id)
                    .count(),
                1
            );
        }
        assert!(stream.messages.is_empty());
    }
}

/// The three events an `LlmAgent` emits when it holds a call: the model's call, the
/// placeholder response, and the event carrying the request.
fn held_call_events() -> Vec<adk_core::Event> {
    let request = adk_core::ToolConfirmationRequest {
        tool_name: "send_invoice".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "invoice": "INV-10" }),
        origin: None,
        acting: None,
    };
    let mut call = response(
        vec![Part::FunctionCall {
            name: "send_invoice".into(),
            args: json!({ "invoice": "INV-10" }),
            id: Some("call-1".into()),
            thought_signature: None,
        }],
        false,
    );
    call.author = "finance".into();
    let mut pending = adk_core::Event::with_id("pending", "invocation");
    pending.author = "finance".into();
    pending.llm_response.content = Some(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "send_invoice",
                adk_core::confirmation_pending_response(
                    "Tool 'send_invoice' requires confirmation",
                ),
            ),
            id: Some("call-1".into()),
            annotations: None,
        }],
    });
    let mut hold = adk_core::Event::with_id("hold", "invocation");
    hold.author = "finance".into();
    hold.llm_response.content = Some(Content::new("model").with_text(
        "Tool confirmation required for 'send_invoice'. Provide approve/deny decision to continue.",
    ));
    hold.actions.tool_confirmation = Some(request);
    vec![call, pending, hold]
}

#[test]
fn a_held_call_is_neither_a_result_nor_a_message_and_is_kept_for_the_interrupt() {
    let mut stream = AgUiStream::default();
    let output: Vec<Value> = held_call_events()
        .iter()
        .flat_map(|event| stream.translate(event, "thread", "run"))
        .collect();
    let kinds: Vec<_> = output.iter().map(|item| item["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["TOOL_CALL_START", "TOOL_CALL_ARGS", "TOOL_CALL_END"]);
    assert_eq!(stream.held.len(), 1);
    assert_eq!(stream.held[0].agent, "finance");
    assert_eq!(stream.held[0].request.function_call_id.as_deref(), Some("call-1"));
}

#[test]
fn a_call_held_again_is_one_interrupt() {
    let mut stream = AgUiStream::default();
    let events = held_call_events();
    stream.translate(&events[2], "thread", "run");
    stream.translate(&events[2], "thread", "run");
    assert_eq!(stream.held.len(), 1);
}

#[test]
fn a_decided_call_reports_its_result_against_the_original_call() {
    let mut stream = AgUiStream::default();
    let mut result = adk_core::Event::with_id("result", "invocation");
    result.llm_response.content = Some(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "send_invoice",
                json!({ "sent": "INV-10" }),
            ),
            id: Some("call-1".into()),
            annotations: None,
        }],
    });
    let output = stream.translate(&result, "thread", "run");
    assert_eq!(output.len(), 1);
    assert_eq!(output[0]["type"], "TOOL_CALL_RESULT");
    assert_eq!(output[0]["toolCallId"], "call-1");
    assert!(stream.held.is_empty());
}
