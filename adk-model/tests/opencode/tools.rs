use super::*;

#[tokio::test]
async fn continues_tool_calls_with_the_same_identity() {
    for &(service, model, api, endpoint) in CASES {
        let server = MockServer::start().await;
        let tool = match api {
            OpenCodeApi::ChatCompletions => {
                let mut reply = chat(model);
                reply["choices"][0] = json!({"index":0,"message":{"role":"assistant","content":null,
                    "reasoning_content":"inspect the file first",
                    "tool_calls":[{"id":"call_read","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]},"finish_reason":"tool_calls"});
                reply
            }
            OpenCodeApi::Responses => {
                let mut reply = response(model);
                reply["output"] = json!([{"type":"function_call","id":"fc_read","call_id":"call_read","name":"read_file","arguments":"{\"path\":\"README.md\"}","status":"completed"}]);
                reply
            }
            OpenCodeApi::GenerateContent => {
                let mut reply = gemini();
                reply["candidates"][0]["content"]["parts"] = json!([{"functionCall":{"id":"call_read","name":"read_file","args":{"path":"README.md"}}}]);
                reply
            }
            OpenCodeApi::Messages => {
                let mut reply = message(model);
                reply["content"] = json!([{"type":"tool_use","id":"call_read","name":"read_file","input":{"path":"README.md"}}]);
                reply["stop_reason"] = json!("tool_use");
                reply
            }
        };
        Mock::given(method("POST"))
            .and(path(format!("/v1/{endpoint}")))
            .and(header("x-opencode-session", "conversation-42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tool))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        let client = OpenCodeClient::new(
            config_for(service, model).with_base_url(format!("{}/v1", server.uri())),
        )
        .unwrap();
        let user = Content::new("user").with_text("read README.md");
        let replies = client
            .generate_content(LlmRequest::new(model, vec![user.clone()]), false)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        let content = replies
            .into_iter()
            .filter_map(|reply| reply.content)
            .find(|content| {
                content.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }))
            })
            .unwrap();
        assert!(content.parts.iter().any(|part| matches!(part,Part::FunctionCall{name,args,id,..} if name=="read_file" && *args==json!({"path":"README.md"}) && id.as_deref()==Some("call_read"))));
        let result = Content {
            role: "function".into(),
            parts: vec![Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    "read_file",
                    json!({"text":"file contents"}),
                ),
                id: Some("call_read".into()),
                annotations: None,
            }],
        };
        let final_reply = reply(api, model);
        Mock::given(method("POST"))
            .and(path(format!("/v1/{endpoint}")))
            .and(header("x-opencode-session", "conversation-42"))
            .and(header("user-agent", "test-coding-agent/1.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(final_reply))
            .expect(1)
            .mount(&server)
            .await;
        client
            .generate_content(LlmRequest::new(model, vec![user, content, result]), false)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        match api {
            OpenCodeApi::ChatCompletions => {
                assert_eq!(body["messages"][1]["reasoning_content"], "inspect the file first");
                assert_eq!(body["messages"][2]["tool_call_id"], "call_read");
            }
            OpenCodeApi::Responses => assert!(body["input"].as_array().unwrap().iter().any(
                |item| item["type"] == "function_call_output" && item["call_id"] == "call_read"
            )),
            OpenCodeApi::GenerateContent => {
                assert!(body["contents"].as_array().unwrap().iter().any(|content| {
                    content["parts"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part["functionResponse"]["name"] == "read_file"
                                && part["functionResponse"]["response"]["text"] == "file contents"
                        })
                    })
                }));
            }
            OpenCodeApi::Messages => {
                assert!(body["messages"].as_array().unwrap().iter().any(|message| {
                    message["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| part["tool_use_id"] == "call_read")
                    })
                }))
            }
        }
    }
}

#[tokio::test]
async fn reports_api_errors_without_changing_protocol() {
    for &(service, model, _api, endpoint) in CASES {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path(format!("/v1/{endpoint}")))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"type":"error","error":{"type":"invalid_request_error","message":"unsupported request"}})))
            .expect(1).mount(&server).await;
        let client = OpenCodeClient::new(
            config_for(service, model).with_base_url(format!("{}/v1", server.uri())),
        )
        .unwrap();
        let result = async {
            client
                .generate_content(
                    LlmRequest::new(model, vec![Content::new("user").with_text("inspect")]),
                    false,
                )
                .await?
                .try_collect::<Vec<_>>()
                .await
        }
        .await;
        assert!(result.is_err());
    }
}
