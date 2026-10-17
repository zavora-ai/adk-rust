#![cfg(feature = "openai")]

// Reference fixtures: earendil-works/pi b06c1f2c (Responses consumed fields,
// raw reasoning, terminal/tool guards); Wei-Shaw/sub2api 92c9433d (wire field
// presence, terminal aliases and authoritative done-item recovery).
use adk_core::{AdkError, Content, FinishReason, Llm, LlmRequest, LlmResponse, Part};
use adk_model::{
    openai::{OpenAIResponsesClient, OpenAIResponsesConfig},
    retry::RetryConfig,
};
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const SECRET: &str = "PRIVATE_WIRE_SENTINEL_prompt_delta_arguments_error";

fn response(output: Value) -> Value {
    json!({"id":"resp_fixture", "status":"completed", "output":output})
}

fn message(text: &str) -> Value {
    json!({"type":"message", "id":"msg", "role":"assistant", "status":"completed",
        "content":[{"type":"output_text", "text":text, "annotations":[]}]})
}

fn terminal(response: Value) -> Value {
    json!({"type":"response.completed", "response":response})
}

async fn generate(body: String, streaming: bool) -> Result<Vec<LlmResponse>, AdkError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                body,
                if streaming { "text/event-stream" } else { "application/json" },
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("fixture-key", "fixture-model").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    let request = LlmRequest::new("fixture-model", vec![Content::new("user").with_text("fixture")]);
    let result =
        client.generate_content(request, streaming).await.unwrap().try_collect::<Vec<_>>().await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&requests[0].body).unwrap()["stream"], streaming);
    result
}

async fn events(events: &[Value]) -> Result<Vec<LlmResponse>, AdkError> {
    generate(events.iter().map(|event| format!("data: {event}\n\n")).collect(), true).await
}

async fn unary(value: &Value) -> Result<Vec<LlmResponse>, AdkError> {
    generate(value.to_string(), false).await
}

fn safe(error: &AdkError) {
    assert!(!error.to_string().contains(SECRET));
    assert!(!format!("{error:?}").contains(SECRET));
    assert!(!serde_json::to_string(&error.details.metadata).unwrap().contains(SECRET));
    assert!(std::error::Error::source(error).is_none());
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn ignores_echoed_metadata_schemas() {
    let mut final_response = response(json!([message("answer")]));
    for field in
        ["tools", "reasoning", "text", "tool_choice", "truncation", "prompt_cache_retention"]
    {
        final_response[field] = json!({"type":"future_vendor_variant", "value":SECRET});
    }
    final_response["service_tier"] = json!("future_tier");
    final_response["usage"] = json!({"input_tokens":5,"output_tokens":3,"total_tokens":8});
    let created = json!({"type":"response.created", "response":{
        "id":"resp_fixture", "tools":[{"type":"future_tool", "arguments":SECRET}],
        "reasoning":{"effort":"vendor_effort"}, "status":"vendor_progress"
    }});
    for result in
        [events(&[created, terminal(final_response.clone())]).await, unary(&final_response).await]
    {
        let results = result.unwrap();
        let last = results.last().unwrap();
        assert_eq!(last.content.as_ref().unwrap().parts[0], Part::Text { text: "answer".into() });
        assert_eq!(
            last.provider_metadata.as_ref().unwrap()["openai"]["service_tier"],
            "future_tier"
        );
        assert_eq!(last.usage_metadata.as_ref().unwrap().total_token_count, 8);
    }
}

#[tokio::test]
async fn tolerates_progress_but_rejects_unknown_output() {
    let progress = json!({"type":"response.vendor.progress", "metadata":SECRET});
    assert!(events(&[progress, terminal(response(json!([])))]).await.is_ok());
    for output in [
        json!([{"type":"future_tool_call", "arguments":SECRET}]),
        json!([{"type":"message", "id":"msg", "role":"assistant", "status":"completed",
            "content":[{"type":"future_content", "text":SECRET}]}]),
    ] {
        let value = response(output);
        for result in [unary(&value).await, events(&[terminal(value.clone())]).await] {
            let error = result.unwrap_err();
            safe(&error);
            assert_eq!(error.code, "model.openai_responses.parse");
            assert_eq!(
                error.details.metadata["openai_responses_parse"]["cause"],
                "unknown_variant"
            );
        }
    }
}

#[tokio::test]
async fn requires_consumed_fields_and_types() {
    let event = json!({"type":"response.output_text.delta", "sequence_number":1,
        "item_id":"msg", "content_index":0, "delta":SECRET});
    let error = events(&[event]).await.unwrap_err();
    safe(&error);
    assert_eq!(
        error.details.metadata["openai_responses_parse"],
        json!({
            "layer":"stream_event","category":"data","line":0,"column":0,
            "cause":"missing_field","field":"output_index","event_type":"response.output_text.delta"
        })
    );
    for event in [
        json!({"type":"response.created", "response":{}}),
        json!({"type":"response.created", "response":{"id":null}}),
        json!({"type":"response.output_text.delta", "sequence_number":1,
            "item_id":"msg", "content_index":0, "delta":SECRET}),
        json!({"type":"response.output_text.delta", "sequence_number":1,
            "item_id":"msg", "output_index":0,"content_index":0, "delta":42}),
    ] {
        let error = events(&[event]).await.unwrap_err();
        safe(&error);
        assert_eq!(error.code, "model.openai_responses.parse");
    }
    for output in [Value::Null, json!(42), json!({})] {
        let value = response(output);
        for result in [unary(&value).await, events(&[terminal(value.clone())]).await] {
            let error = result.unwrap_err();
            safe(&error);
            assert_eq!(error.code, "model.openai_responses.parse");
        }
    }
    // Usage counts are read leniently: a missing total is the sum of the reported counts,
    // and the usage object is kept as received.
    let mut value = response(json!([]));
    value["usage"] = json!({"input_tokens":1,"output_tokens":2});
    let usage = unary(&value).await.unwrap().pop().unwrap().usage_metadata.unwrap();
    assert_eq!(usage.total_token_count, 3);
    assert_eq!(usage.provider_usage, Some(value["usage"].clone()));
}

#[tokio::test]
async fn recovers_only_authoritative_missing_output() {
    let missing = json!({"id":"resp_fixture", "status":"completed"});
    assert_eq!(unary(&missing).await.unwrap_err().code, "model.openai_responses.parse");
    assert_eq!(
        events(&[terminal(missing.clone())]).await.unwrap_err().code,
        "model.openai_responses.invalid_terminal"
    );
    let done = json!({"type":"response.output_item.done", "sequence_number":1,
        "output_index":0, "item":message("recovered")});
    let results = events(&[done, terminal(missing)]).await.unwrap();
    assert_eq!(
        results.last().unwrap().content.as_ref().unwrap().parts[0],
        Part::Text { text: "recovered".into() }
    );
    assert!(unary(&response(json!([]))).await.is_ok());
}

#[tokio::test]
async fn rejects_empty_terminal_identity() {
    let value = json!({"id":"", "status":"completed", "output":[]});
    for result in [unary(&value).await, events(&[terminal(value.clone())]).await] {
        let error = result.unwrap_err();
        safe(&error);
        assert_eq!(error.code, "model.openai_responses.invalid_identity");
    }
}

#[tokio::test]
async fn rejects_unrelated_done_after_tool_fragment() {
    let error = events(&[
        json!({"type":"response.function_call_arguments.delta", "sequence_number":1,
            "item_id":"fc_pending", "output_index":0, "delta":"{\"path\":"}),
        json!({"type":"response.output_item.done", "sequence_number":2,
            "output_index":0, "item":message("unrelated")}),
        terminal(response(json!([message("unrelated")]))),
    ])
    .await
    .unwrap_err();
    safe(&error);
    assert_eq!(error.code, "model.openai_responses.unfinished_stream");
}

#[tokio::test]
async fn rejects_unfinished_done_identity_replacement() {
    for (status, arguments, call_id) in
        [("in_progress", "{}", "call_b"), ("completed", "{", "call_b"), ("completed", "{}", "")]
    {
        let done_call = json!({"type":"function_call", "call_id":call_id,
            "name":"read", "arguments":arguments, "status":status});
        let final_call = json!({"type":"function_call", "call_id":"call_b",
            "name":"read", "arguments":"{}", "status":"completed"});
        let error = events(&[
            json!({"type":"response.output_item.added", "sequence_number":1,
                "output_index":0, "item":{"type":"function_call", "call_id":"call_a",
                    "name":"read", "arguments":"", "status":"in_progress"}}),
            json!({"type":"response.output_item.done", "sequence_number":2,
                "output_index":0, "item":done_call}),
            terminal(response(json!([final_call]))),
        ])
        .await
        .unwrap_err();
        safe(&error);
        assert_eq!(error.code, "model.openai_responses.unfinished_stream");
    }
}

#[tokio::test]
async fn preserves_raw_reasoning_and_done_order() {
    let reasoning = json!({"type":"reasoning", "id":"rs", "summary":[],
        "content":[{"type":"reasoning_text", "text":"thought"}]});
    let streamed = events(&[
        json!({"type":"response.reasoning_text.delta", "sequence_number":1,
            "item_id":"rs","output_index":0,"content_index":0,"delta":"tho"}),
        json!({"type":"response.reasoning_text.done", "sequence_number":2,
            "item_id":"rs","output_index":0,"content_index":0,"text":"thought"}),
        json!({"type":"response.output_item.done", "sequence_number":3,
            "output_index":0,"item":reasoning.clone()}),
        terminal(response(json!([]))),
    ])
    .await
    .unwrap();
    let body = unary(&response(json!([reasoning]))).await.unwrap();
    assert_eq!(
        serde_json::to_value(&streamed.last().unwrap().content).unwrap(),
        serde_json::to_value(&body.last().unwrap().content).unwrap()
    );
    assert_eq!(
        body[0].content.as_ref().unwrap().parts[0],
        Part::Thinking { thinking: "thought".into(), signature: None }
    );
}

#[tokio::test]
async fn preserves_parallel_arguments_and_rejects_truncation() {
    let calls = json!([
        {"type":"function_call","id":"fc_a","call_id":"call_a","name":"read",
            "arguments":"{ \"path\" : \"a\" }"},
        {"type":"function_call","id":"fc_b","call_id":"call_b","name":"read",
            "arguments":"{\"path\":\"b\"}"}
    ]);
    for results in [
        unary(&response(calls.clone())).await.unwrap(),
        events(&[terminal(response(calls))]).await.unwrap(),
    ] {
        let parts = &results.last().unwrap().content.as_ref().unwrap().parts;
        for (part, path, id) in [(&parts[0], "a", "call_a"), (&parts[1], "b", "call_b")] {
            let Part::FunctionCall { args, id: call_id, .. } = part else {
                panic!("function call expected")
            };
            assert_eq!(args["path"], path);
            assert_eq!(call_id.as_deref(), Some(id));
        }
    }
    let value = response(json!([{"type":"function_call","call_id":"call_bad",
        "name":"read","arguments":"{\"path\":"}]));
    for result in [unary(&value).await.unwrap(), events(&[terminal(value.clone())]).await.unwrap()]
    {
        let last = result.last().unwrap();
        assert!(last.error_code.is_none());
        let parts = &last.content.as_ref().expect("call content").parts;
        assert!(matches!(
            parts.as_slice(),
            [Part::FunctionCall { args, .. }] if adk_core::invalid_tool_arguments_reason(args).is_some()
        ));
    }
    for output in [
        json!([{"type":"function_call","call_id":"same","name":"read","arguments":"{}"},
            {"type":"function_call","call_id":"same","name":"read","arguments":"{}"}]),
        json!([{"type":"function_call","call_id":"","name":"read","arguments":"{}"}]),
        json!([{"type":"function_call","call_id":"call","name":"read","arguments":"{}","status":"in_progress"}]),
    ] {
        let value = response(output);
        for error in [
            unary(&value).await.unwrap_err(),
            events(&[terminal(value.clone())]).await.unwrap_err(),
        ] {
            safe(&error);
        }
    }
}

#[tokio::test]
async fn rejects_failed_unknown_and_nonterminal_statuses() {
    for status in ["failed", "cancelled", "queued", "in_progress", SECRET] {
        let mut value = response(json!([]));
        value["status"] = json!(status);
        value["error"] = json!({"code":SECRET,"message":SECRET});
        for result in [unary(&value).await, events(&[terminal(value.clone())]).await] {
            let error = result.unwrap_err();
            safe(&error);
            assert_ne!(error.code, "model.openai_responses.unfinished_stream");
        }
    }
    for reason in [None, Some(SECRET)] {
        let mut value = response(json!([]));
        value["status"] = json!("incomplete");
        value["incomplete_details"] =
            reason.map(|reason| json!({"reason":reason})).unwrap_or(Value::Null);
        for error in [
            unary(&value).await.unwrap_err(),
            events(&[terminal(value.clone())]).await.unwrap_err(),
        ] {
            safe(&error);
            assert_eq!(error.code, "model.openai_responses.incomplete");
        }
    }
}

#[tokio::test]
async fn failures_keep_documented_codes_and_their_categories() {
    use adk_core::ErrorCategory;

    for (code, category) in [
        ("rate_limit_exceeded", ErrorCategory::RateLimited),
        ("server_error", ErrorCategory::Unavailable),
        ("vector_store_timeout", ErrorCategory::Timeout),
        ("invalid_image_url", ErrorCategory::InvalidInput),
    ] {
        let mut failed = response(json!([]));
        failed["status"] = json!("failed");
        failed["error"] = json!({"code":code, "message":SECRET});
        for error in [
            unary(&failed).await.unwrap_err(),
            events(&[json!({"type":"response.failed", "response":failed})]).await.unwrap_err(),
            events(&[json!({"type":"error", "code":code, "message":SECRET})]).await.unwrap_err(),
        ] {
            assert_eq!(error.code, "model.openai_responses.response_failed");
            assert_eq!(error.category, category, "{code}");
            assert_eq!(error.is_retryable(), category != ErrorCategory::InvalidInput, "{code}");
            assert_eq!(error.message, format!("OpenAI Responses generation failed: {code}"));
            assert_eq!(error.details.metadata["provider_error_code"], json!(code));
            assert!(!format!("{error:?}").contains(SECRET));
        }
    }
}

#[tokio::test]
async fn preserves_known_incomplete_outcomes_and_aliases() {
    for (reason, finish) in
        [("max_output_tokens", FinishReason::MaxTokens), ("content_filter", FinishReason::Safety)]
    {
        let mut value = response(json!([]));
        value["status"] = json!("incomplete");
        value["incomplete_details"] = json!({"reason":reason});
        for result in
            [unary(&value).await.unwrap(), events(&[terminal(value.clone())]).await.unwrap()]
        {
            assert_eq!(result.last().unwrap().finish_reason, Some(finish));
        }
    }
    let mut done = terminal(response(json!([])));
    done["type"] = json!("response.done");
    assert!(events(&[done]).await.is_ok());
    for kind in ["response.cancelled", "response.canceled"] {
        let error = events(&[json!({"type":kind})]).await.unwrap_err();
        safe(&error);
        assert_eq!(error.code, "model.openai_responses.cancelled");
    }
}

#[tokio::test]
async fn retries_eof_without_terminal() {
    for prefix in ["", ": heartbeat\n\n", "data: [DONE]\n\n"] {
        let error = generate(prefix.into(), true).await.unwrap_err();
        assert_eq!(error.category, adk_core::ErrorCategory::Unavailable);
        assert!(error.retry.should_retry);
        assert!(adk_model::retry::is_retryable_model_error(&error));
        assert_eq!(error.code, "model.openai_responses.unfinished_stream");
    }
    let error =
        events(&[json!({"type":"response.created","response":{"id":"r"}})]).await.unwrap_err();
    assert_eq!(error.category, adk_core::ErrorCategory::Unavailable);
    assert!(error.retry.should_retry);
    assert_eq!(error.code, "model.openai_responses.unfinished_stream");
}

#[tokio::test]
async fn rejects_unfinished_terminal() {
    let fragment = json!({"type":"response.function_call_arguments.delta", "sequence_number":1,
        "item_id":"fc", "output_index":0, "delta":"{"});
    let error = events(&[fragment, terminal(response(json!([])))]).await.unwrap_err();
    safe(&error);
    assert_eq!(error.category, adk_core::ErrorCategory::Internal);
    assert!(!error.retry.should_retry);
    assert_eq!(error.code, "model.openai_responses.unfinished_stream");
}

#[tokio::test]
async fn accepts_authoritative_tool_completion() {
    let added = json!({"type":"response.output_item.added","sequence_number":1,"output_index":0,
        "item":{"type":"function_call","id":"fc","call_id":"call","name":"read",
            "arguments":"","status":"in_progress"}});
    let call = json!({"type":"function_call","id":"fc","call_id":"call","name":"read",
        "arguments":"{ \"path\" : \"a\" }","status":"completed"});
    let completed = response(json!([call]));
    let results = events(&[added.clone(), terminal(completed)]).await.unwrap();
    let Part::FunctionCall { id, args, .. } =
        &results.last().unwrap().content.as_ref().unwrap().parts[0]
    else {
        panic!("function call expected")
    };
    assert_eq!(id.as_deref(), Some("call"));
    assert_eq!(args["path"], "a");
    for value in [response(json!([])), json!({"id":"r","status":"completed"})] {
        let error = events(&[added.clone(), terminal(value)]).await.unwrap_err();
        safe(&error);
    }
}
