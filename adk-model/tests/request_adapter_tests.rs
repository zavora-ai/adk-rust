#![cfg(feature = "openai")]

use adk_core::{Content, Llm, LlmRequest};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig, RequestAdapter};
use adk_model::retry::RetryConfig;
use adk_model::{OpenAICompatible, OpenAICompatibleConfig};
use futures::TryStreamExt;
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method},
};

#[tokio::test]
async fn request_customization_preserves_disabled_retries() {
    for responses in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("x-fixture", "custom"))
            .respond_with(ResponseTemplate::new(503).set_body_json(
                serde_json::json!({"error":{"message":"retry fixture","type":"server_error"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let adapter: RequestAdapter = Arc::new(|body, headers| {
            body["metadata"] = serde_json::json!({"fixture":"custom"});
            headers.insert("x-fixture", "custom".parse().unwrap());
            Ok(())
        });
        let model: Box<dyn Llm> = if responses {
            Box::new(
                OpenAIResponsesClient::new(
                    OpenAIResponsesConfig::new("fixture", "fixture").with_base_url(server.uri()),
                )
                .unwrap()
                .with_retry_config(RetryConfig::disabled())
                .with_request_adapter(adapter)
                .unwrap(),
            )
        } else {
            Box::new(
                OpenAICompatible::new(
                    OpenAICompatibleConfig::new("fixture", "fixture").with_base_url(server.uri()),
                )
                .unwrap()
                .with_retry_config(RetryConfig::disabled())
                .with_request_adapter(adapter),
            )
        };
        let request = LlmRequest::new("fixture", vec![Content::new("user").with_text("Hello")]);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut stream = model.generate_content(request, false).await?;
            stream.try_next().await
        })
        .await
        .expect("disabled retries must return the first HTTP failure");
        assert!(result.is_err());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["metadata"]["fixture"], "custom");
    }
}
