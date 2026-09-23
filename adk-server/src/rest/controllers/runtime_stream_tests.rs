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
