#![cfg(feature = "openai")]

use adk_core::{Content, Llm, LlmRequest, LlmResponse, Part};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn completion(output: Value) -> Value {
    json!({
        "type": "response.completed", "sequence_number": 10,
        "response": {
            "id": "resp_test", "object": "response", "created_at": 0,
            "model": "test-model", "status": "completed", "output": output,
            "usage": {
                "input_tokens": 100, "input_tokens_details": {"cached_tokens": 42},
                "output_tokens": 20, "output_tokens_details": {"reasoning_tokens": 5},
                "total_tokens": 120
            }
        }
    })
}

fn message(id: &str, content: Value) -> Value {
    json!({
        "type": "message", "id": id, "role": "assistant", "status": "completed",
        "content": content
    })
}

fn text(value: &str) -> Value {
    json!({"type": "output_text", "text": value, "annotations": []})
}

fn delta(id: &str, output_index: u32, content_index: u32, value: &str) -> Value {
    json!({
        "type": "response.output_text.delta", "sequence_number": 1, "item_id": id,
        "output_index": output_index, "content_index": content_index, "delta": value
    })
}

fn done(id: &str, output_index: u32, content_index: u32, value: &str) -> Value {
    json!({
        "type": "response.output_text.done", "sequence_number": 2, "item_id": id,
        "output_index": output_index, "content_index": content_index,
        "text": value, "logprobs": []
    })
}

fn text_parts(results: &[LlmResponse]) -> Vec<&str> {
    results
        .iter()
        .filter_map(|response| response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn responses(events: &[Value], open_responses_mode: bool, prefix: &str) -> Vec<LlmResponse> {
    try_responses(events, open_responses_mode, prefix).await.expect("SSE events must deserialize")
}

async fn try_responses(
    events: &[Value],
    open_responses_mode: bool,
    prefix: &str,
) -> Result<Vec<LlmResponse>, adk_core::AdkError> {
    let server = MockServer::start().await;
    let mut body = prefix.to_string();
    for event in events {
        body.push_str(&format!("data: {event}\n\n"));
    }
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let client = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("test-key", "test-model")
            .with_base_url(server.uri())
            .with_open_responses_mode(open_responses_mode),
    )
    .unwrap();
    let request = LlmRequest::new("test-model", vec![Content::new("user").with_text("read files")]);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        client.generate_content(request, true).await.unwrap().try_collect::<Vec<_>>().await
    })
    .await
    .expect("stream must terminate");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&requests[0].body).unwrap()["stream"], true);
    result
}

#[tokio::test]
async fn compatible_stream_rejects_conflicting_tool_identities_before_completion() {
    for conflict in [
        json!({"type":"response.output_item.added", "sequence_number":3, "output_index":0,
            "item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file","arguments":"","status":"in_progress"}}),
        json!({"type":"response.output_item.done", "sequence_number":3, "output_index":0,
            "item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file","arguments":"","status":"completed"}}),
        json!({"type":"response.function_call_arguments.delta", "sequence_number":3, "output_index":0,
            "item_id":"fc_b","delta":" "}),
        json!({"type":"response.function_call_arguments.done", "sequence_number":3, "output_index":0,
            "item_id":"fc_b","arguments":""}),
    ] {
        let events = [
            json!({"type":"response.output_item.added", "sequence_number":1, "output_index":0,
                "item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"read_file","arguments":"","status":"in_progress"}}),
            json!({"type":"response.function_call_arguments.delta", "sequence_number":2, "output_index":0,
                "item_id":"fc_a","delta":r#"{"path":"A"}"#}),
            conflict.clone(),
            json!({"type":"response.output_item.done", "sequence_number":4, "output_index":0,
                "item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file","arguments":"","status":"completed"}}),
            completion(
                json!([{"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file"}]),
            ),
        ];
        assert!(
            try_responses(&events, true, "").await.is_err(),
            "conflicting event was accepted: {conflict}"
        );
    }
}

#[tokio::test]
async fn compatible_stream_restores_blank_done_and_completed_arguments() {
    for blank in ["", " \t\r\n"] {
        let results = responses(&[
            json!({"type":"response.function_call_arguments.delta", "sequence_number":1, "output_index":0,
                "item_id":"fc_a","delta":r#"{"path":"first.txt"}"#}),
            json!({"type":"response.function_call_arguments.done", "sequence_number":2, "output_index":0,
                "item_id":"fc_a","arguments":blank}),
            json!({"type":"response.output_item.done", "sequence_number":3, "output_index":0,
                "item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"read_file","arguments":blank,"status":"completed"}}),
            completion(json!([{"type":"function_call","id":"fc_a","call_id":"call_a","name":"read_file","arguments":blank}])),
        ], true, "").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].error_code, None);
        assert_eq!(
            results[0].content.as_ref().unwrap().parts,
            vec![Part::FunctionCall {
                id: Some("call_a".into()),
                name: "read_file".into(),
                args: json!({"path":"first.txt"}),
                thought_signature: None,
            }]
        );
        assert_eq!(
            results[0].usage_metadata.as_ref().unwrap().cache_read_input_token_count,
            Some(42)
        );
    }
}

#[tokio::test]
async fn standard_mode_does_not_apply_compatibility_identity_rules() {
    let results = responses(&[
        json!({"type":"response.output_item.added", "sequence_number":1, "output_index":0,
            "item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"read_file","arguments":"","status":"in_progress"}}),
        json!({"type":"response.function_call_arguments.delta", "sequence_number":2, "output_index":0,
            "item_id":"fc_a","delta":r#"{"path":"first.txt"}"#}),
        json!({"type":"response.output_item.done", "sequence_number":3, "output_index":0,
            "item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file","arguments":r#"{"path":"second.txt"}"#,"status":"completed"}}),
        completion(json!([{"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file",
            "arguments":r#"{"path":"second.txt"}"#,"status":"completed"}])),
    ], false, "").await;
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].content.as_ref().unwrap().parts,
        vec![Part::FunctionCall {
            id: Some("call_b".into()),
            name: "read_file".into(),
            args: json!({"path":"second.txt"}),
            thought_signature: None,
        }]
    );
}

#[tokio::test]
async fn streamed_invalid_arguments_are_marked_and_keep_usage_in_both_modes() {
    for compatible in [false, true] {
        let results = responses(
            &[completion(json!([{
                "type": "function_call", "id": "fc_bad", "call_id": "call_bad",
                "name": "read_file", "arguments": "{invalid", "status": "completed"
            }]))],
            compatible,
            "",
        )
        .await;
        assert_eq!(results.len(), 1);
        let result = &results[0];
        // The call is marked for the agent to answer with an error, not a failed turn.
        assert!(result.error_code.is_none());
        let parts = &result.content.as_ref().expect("call content").parts;
        assert!(matches!(
            parts.as_slice(),
            [Part::FunctionCall { args, .. }] if adk_core::invalid_tool_arguments_reason(args).is_some()
        ));
        assert!(result.turn_complete);
        assert!(!result.partial);
        assert_eq!(result.usage_metadata.as_ref().unwrap().cache_read_input_token_count, Some(42));
        assert_eq!(
            result.provider_metadata.as_ref().unwrap()["openai"]["response_id"],
            "resp_test"
        );
    }
}

#[tokio::test]
async fn compatible_stream_restores_only_the_matching_tools_arguments() {
    let results = responses(
        &[
            json!({
                "type": "response.function_call_arguments.delta", "sequence_number": 1,
                "output_index": 1, "item_id": "fc_b", "delta": r#"{"path":"second.txt"}"#
            }),
            completion(json!([
                {"type": "function_call", "id": "fc_a", "call_id": "call_a",
                    "name": "read_file", "arguments": r#"{"path":"first.txt"}"#},
                {"type": "function_call", "id": "fc_b", "call_id": "call_b", "name": "read_file"}
            ])),
        ],
        true,
        "data:\n\ndata: \r\n\r\n: keepalive\n\n",
    )
    .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].error_code, None);
    assert_eq!(
        results[0].content.as_ref().unwrap().parts,
        vec![
            Part::FunctionCall {
                id: Some("call_a".into()),
                name: "read_file".into(),
                args: json!({"path": "first.txt"}),
                thought_signature: None
            },
            Part::FunctionCall {
                id: Some("call_b".into()),
                name: "read_file".into(),
                args: json!({"path": "second.txt"}),
                thought_signature: None
            },
        ]
    );
}

mod output_recovery {
    use super::*;

    #[tokio::test]
    async fn completed_text_without_deltas_is_visible() {
        for compatible in [false, true] {
            let results = responses(
                &[completion(json!([
                    message("first", json!([text("First")])),
                    message("second", json!([text("Second")]))
                ]))],
                compatible,
                "",
            )
            .await;
            assert_eq!(text_parts(&results), ["First", "Second"]);
            assert!(results[0].turn_complete);
            assert_eq!(results[0].usage_metadata.as_ref().unwrap().total_token_count, 120);
        }
    }

    #[tokio::test]
    async fn final_events_emit_only_unseen_suffixes() {
        for compatible in [false, true] {
            let item = message("msg", json!([text("Answer")]));
            let results = responses(
                &[
                    delta("msg", 0, 0, "Ans"),
                    done("msg", 0, 0, "Answer"),
                    done("msg", 0, 0, "Answer"),
                    json!({"type":"response.content_part.done", "sequence_number":3,
                        "item_id":"msg", "output_index":0, "content_index":0,
                        "part":text("Answer")}),
                    json!({"type":"response.output_item.done", "sequence_number":4,
                        "output_index":0, "item":item}),
                    completion(json!([item])),
                ],
                compatible,
                "",
            )
            .await;
            assert_eq!(text_parts(&results), ["Ans", "wer"]);
            assert!(results.last().unwrap().content.is_none());
            assert_eq!(
                results
                    .last()
                    .unwrap()
                    .usage_metadata
                    .as_ref()
                    .unwrap()
                    .cache_read_input_token_count,
                Some(42)
            );
        }
    }

    #[tokio::test]
    async fn done_recovers_missing_terminal_output() {
        for compatible in [false, true] {
            let item = json!({"type":"function_call", "id":"fc_a", "call_id":"call_a",
                "name":"read_file", "arguments":r#"{"path":"first.txt"}"#, "status":"completed"});
            let results = responses(
                &[
                    json!({"type":"response.output_item.done", "sequence_number":1,
                        "output_index":0, "item":item}),
                    json!({"type":"response.output_item.done", "sequence_number":2,
                        "output_index":0, "item":item}),
                    completion(json!([])),
                ],
                compatible,
                "",
            )
            .await;
            let terminal = results.last().unwrap();
            assert_eq!(terminal.provider_metadata.as_ref().unwrap()["content_complete"], true);
            assert_eq!(
                terminal.content.as_ref().unwrap().parts,
                [Part::FunctionCall {
                    id: Some("call_a".into()),
                    name: "read_file".into(),
                    args: json!({"path":"first.txt"}),
                    thought_signature: None,
                }]
            );
        }
    }

    #[tokio::test]
    async fn explicit_terminal_output_supersedes_done_items() {
        for compatible in [false, true] {
            let item = json!({"type":"function_call", "id":"fc_a", "call_id":"call_a",
                "name":"read_file", "arguments":"{}", "status":"completed"});
            let results = responses(
                &[
                    json!({"type":"response.output_item.done", "sequence_number":1,
                        "output_index":0, "item":item}),
                    completion(json!([message("answer", json!([text("No tool needed")]))])),
                ],
                compatible,
                "",
            )
            .await;
            assert_eq!(text_parts(&results), ["No tool needed"]);
            assert!(
                !results
                    .last()
                    .unwrap()
                    .content
                    .as_ref()
                    .unwrap()
                    .parts
                    .iter()
                    .any(|part| matches!(part, Part::FunctionCall { .. }))
            );
        }
    }

    #[tokio::test]
    async fn corrected_and_unfinished_text_use_authoritative_output() {
        for compatible in [false, true] {
            let corrected = responses(
                &[
                    delta("msg", 0, 0, "Draft"),
                    done("msg", 0, 0, "Final"),
                    completion(json!([message("msg", json!([text("Final")]))])),
                ],
                compatible,
                "",
            )
            .await;
            assert_eq!(text_parts(&corrected), ["Draft", "Final"]);
            let terminal = corrected.last().unwrap();
            assert_eq!(terminal.provider_metadata.as_ref().unwrap()["content_complete"], true);
            assert_eq!(
                terminal.content.as_ref().unwrap().parts,
                [Part::Text { text: "Final".into() }]
            );

            let unfinished =
                responses(&[delta("msg", 0, 0, "Draft"), completion(json!([]))], compatible, "")
                    .await;
            let terminal = unfinished.last().unwrap();
            assert_eq!(terminal.provider_metadata.as_ref().unwrap()["content_complete"], true);
            assert_eq!(
                terminal.content.as_ref().unwrap().parts,
                [Part::Text { text: "Draft".into() }]
            );
        }
    }

    #[tokio::test]
    async fn empty_terminal_output_keeps_streamed_text() {
        for compatible in [false, true] {
            let results = responses(
                &[
                    delta("first", 0, 0, "Hel"),
                    delta("first", 0, 0, "lo"),
                    delta("second", 1, 0, "Dra"),
                    done("second", 1, 0, "Done"),
                    completion(json!([])),
                ],
                compatible,
                "",
            )
            .await;
            let terminal = results.last().unwrap();
            assert!(terminal.turn_complete);
            assert_eq!(terminal.provider_metadata.as_ref().unwrap()["content_complete"], true);
            assert_eq!(
                terminal.content.as_ref().unwrap().parts,
                [Part::Text { text: "Hello".into() }, Part::Text { text: "Done".into() }]
            );
        }
    }

    #[tokio::test]
    async fn out_of_order_parts_use_terminal_order() {
        for compatible in [false, true] {
            let results = responses(
                &[
                    delta("second", 1, 0, "Second"),
                    delta("first", 0, 0, "First"),
                    completion(json!([
                        message("first", json!([text("First")])),
                        message("second", json!([text("Second")]))
                    ])),
                ],
                compatible,
                "",
            )
            .await;
            let terminal = results.last().unwrap();
            assert_eq!(terminal.provider_metadata.as_ref().unwrap()["content_complete"], true);
            assert_eq!(
                terminal.content.as_ref().unwrap().parts,
                [Part::Text { text: "First".into() }, Part::Text { text: "Second".into() },]
            );
        }
    }
}

mod refusal_recovery {
    use super::*;

    #[tokio::test]
    async fn repeated_payloads_emit_only_unseen_suffixes() {
        for compatible in [false, true] {
            let refusal = json!({"type":"refusal", "refusal":"I cannot help"});
            let item = message("msg", json!([refusal]));
            let results = responses(
                &[
                    json!({"type":"response.refusal.delta", "sequence_number":1,
                        "item_id":"msg", "output_index":0, "content_index":0,
                        "delta":"I cannot"}),
                    json!({"type":"response.refusal.done", "sequence_number":2,
                        "item_id":"msg", "output_index":0, "content_index":0,
                        "refusal":"I cannot help"}),
                    json!({"type":"response.content_part.done", "sequence_number":3,
                        "item_id":"msg", "output_index":0, "content_index":0,
                        "part":refusal}),
                    json!({"type":"response.output_item.done", "sequence_number":4,
                        "output_index":0, "item":item}),
                    completion(json!([item])),
                ],
                compatible,
                "",
            )
            .await;
            assert_eq!(text_parts(&results), ["I cannot", " help"]);
        }
    }

    #[tokio::test]
    async fn terminal_refusal_keeps_following_citation_offsets() {
        for compatible in [false, true] {
            let results = responses(
                &[completion(json!([message(
                    "msg",
                    json!([
                        {"type":"refusal", "refusal":"Cannot"},
                        {"type":"output_text", "text":"Details", "annotations":[{
                            "type":"url_citation", "url":"https://example.test", "title":"Example",
                            "start_index":0, "end_index":7
                        }]}
                    ])
                )]))],
                compatible,
                "",
            )
            .await;
            assert_eq!(text_parts(&results), ["Cannot", "Details"]);
            let citation = &results[0].citation_metadata.as_ref().unwrap().citation_sources[0];
            assert_eq!((citation.start_index, citation.end_index), (Some(6), Some(13)));
        }
    }
}

#[tokio::test]
async fn reasoning_summary_done_emits_only_unseen_text() {
    for compatible in [false, true] {
        let item = json!({"type":"reasoning", "id":"rs", "status":"completed", "summary":[
            {"type":"summary_text", "text":"Thinking"},
            {"type":"summary_text", "text":"More"}
        ]});
        let results = responses(
            &[
                json!({"type":"response.reasoning_summary_text.delta", "sequence_number":1,
                    "item_id":"rs", "output_index":0, "summary_index":0, "delta":"Think"}),
                json!({"type":"response.reasoning_summary_text.done", "sequence_number":2,
                    "item_id":"rs", "output_index":0, "summary_index":0, "text":"Thinking"}),
                json!({"type":"response.reasoning_summary_part.done", "sequence_number":3,
                    "item_id":"rs", "output_index":0, "summary_index":0,
                    "part":{"type":"summary_text", "text":"Thinking"}}),
                json!({"type":"response.output_item.done", "sequence_number":4,
                    "output_index":0, "item":item}),
                completion(json!([item, message("msg", json!([text("Answer")]))])),
            ],
            compatible,
            "",
        )
        .await;
        let thinking = results
            .iter()
            .filter_map(|response| response.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                Part::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(thinking, ["Think", "ing", "More"]);
        assert_eq!(text_parts(&results), ["Answer"]);
    }
}

#[tokio::test]
async fn native_tool_requests_stream_and_preserve_final_call() {
    for compatible in [false, true] {
        let server = MockServer::start().await;
        let tool = json!({
            "type":"web_search_call", "id":"ws_1", "status":"completed",
            "action":{"type":"search", "query":"rust"}
        });
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: {}\n\n",
            json!({"type":"response.output_item.added", "sequence_number":0,
                "output_index":0, "item":{"type":"web_search_call", "id":"ws_1",
                "status":"in_progress"}}),
            json!({"type":"response.web_search_call.searching", "sequence_number":1,
                "output_index":0, "item_id":"ws_1"}),
            json!({"type":"response.output_item.done", "sequence_number":2,
                "output_index":0, "item":tool}),
            completion(json!([tool, message("answer", json!([text("Found results")]))]))
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;
        let client = OpenAIResponsesClient::new(
            OpenAIResponsesConfig::new("test-key", "test-model")
                .with_base_url(server.uri())
                .with_open_responses_mode(compatible),
        )
        .unwrap();
        let mut request =
            LlmRequest::new("test-model", vec![Content::new("user").with_text("Search")]);
        request.tools.insert("search".into(), json!({"x-adk-openai-tool":{"type":"web_search"}}));
        let results = client
            .generate_content(request, true)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        let sent = server.received_requests().await.unwrap();
        let request_body: Value = serde_json::from_slice(&sent[0].body).unwrap();
        assert_eq!(request_body["stream"], true);
        assert_eq!(results.len(), 1);
        let terminal = &results[0];
        assert!(terminal.turn_complete);
        assert_eq!(terminal.provider_metadata.as_ref().unwrap()["content_complete"], true);
        assert!(matches!(
            &terminal.content.as_ref().unwrap().parts[0],
            Part::ServerToolCall { .. }
        ));
        assert_eq!(
            terminal.content.as_ref().unwrap().parts[1],
            Part::Text { text: "Found results".into() }
        );
    }
}

#[tokio::test]
async fn unknown_event_types_do_not_end_the_stream() {
    for compatible in [false, true] {
        let results = responses(
            &[
                delta("msg", 0, 0, "Hello"),
                json!({"type": "response.future_tool.progress", "sequence_number": 2,
                    "output_index": 0, "item_id": "ft_1"}),
                delta("msg", 0, 0, " world"),
                completion(json!([message("msg", json!([text("Hello world")]))])),
            ],
            compatible,
            "",
        )
        .await;
        assert_eq!(text_parts(&results), ["Hello", " world"]);
        let terminal = results.last().unwrap();
        assert!(terminal.turn_complete);
        assert_eq!(terminal.usage_metadata.as_ref().unwrap().total_token_count, 120);
    }
}

#[tokio::test]
async fn malformed_known_events_still_fail_the_stream() {
    let error = try_responses(
        &[
            json!({"type": "response.output_text.delta", "sequence_number": 1}),
            completion(json!([message("msg", json!([text("Hello")]))])),
        ],
        false,
        "",
    )
    .await
    .expect_err("a known event missing required fields must fail");
    assert_eq!(error.code, "model.openai_responses.parse");
}
