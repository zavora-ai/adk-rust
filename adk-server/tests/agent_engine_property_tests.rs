//! Property tests for the Agent Engine dispatcher: arbitrary `class_method`
//! strings and arbitrary JSON `input` values must produce an HTTP response,
//! never a panic.

#![cfg(feature = "agent-engine")]

use adk_server::agent_engine::{AgentEngineState, agent_engine_router};
use async_stream::stream;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use proptest::prelude::*;
use std::sync::Arc;
use tower::ServiceExt;

struct EchoAgent;

#[async_trait]
impl adk_core::Agent for EchoAgent {
    fn name(&self) -> &str {
        "echo-agent"
    }

    fn description(&self) -> &str {
        "Echo agent for dispatcher property tests"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let s = stream! {
            yield Ok(adk_core::Event::new("prop-invocation"));
        };
        Ok(Box::pin(s))
    }
}

fn build_router() -> axum::Router {
    let session_service = Arc::new(adk_session::InMemorySessionService::new());
    let runner = Arc::new(
        adk_runner::Runner::builder()
            .app_name("prop-app")
            .agent(Arc::new(EchoAgent))
            .session_service(session_service)
            .build()
            .expect("runner builds"),
    );
    agent_engine_router(AgentEngineState::new(runner))
}

/// Shallow arbitrary JSON: enough shape variety to hit every deserialization
/// branch without generating megabyte documents.
fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        "[a-zA-Z0-9_ -]{0,20}".prop_map(serde_json::Value::from),
    ];
    leaf.prop_recursive(3, 32, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::Array),
            prop::collection::hash_map("[a-z_]{1,12}", inner, 0..6)
                .prop_map(|map| { serde_json::Value::Object(map.into_iter().collect()) }),
        ]
    })
}

/// Class-method names: mostly valid contract names, sometimes garbage.
fn arb_class_method() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::sample::select(vec![
            "create_session",
            "async_create_session",
            "get_session",
            "async_get_session",
            "list_sessions",
            "async_list_sessions",
            "delete_session",
            "async_delete_session",
            "stream_query",
            "async_stream_query",
            "streaming_agent_run_with_events",
            "async_add_session_to_memory",
            "async_search_memory",
            "register_operations",
        ])
        .prop_map(str::to_string),
        1 => "[a-zA-Z0-9_.:-]{0,32}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn dispatcher_never_panics(class_method in arb_class_method(), input in arb_json()) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let app = build_router();
            let body = serde_json::json!({
                "class_method": class_method,
                "input": input,
            });
            for uri in ["/api/reasoning_engine", "/api/stream_reasoning_engine"] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(uri)
                            .header("content-type", "application/json")
                            .body(Body::from(serde_json::to_string(&body).unwrap()))
                            .unwrap(),
                    )
                    .await
                    .expect("dispatcher always responds");
                // Streams must also drain without panicking.
                let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await;
            }
        });
    }
}
