//! Live invocation of a deployed Agent Engine via `:streamQuery?alt=sse`.
//!
//! Requires ADC plus `VERTEX_REMOTE_ENGINE` (a full
//! `projects/*/locations/*/reasoningEngines/*` resource name of an engine
//! deployed with the ADK class-method contract, e.g. via Wave 2's
//! `adk-rust deploy agent-engine`).

#![cfg(feature = "vertex-remote-engine")]

use adk_server::agent_engine::remote::RemoteReasoningEngineAgent;

#[tokio::test]
#[ignore = "requires ADC and VERTEX_REMOTE_ENGINE"]
async fn remote_engine_live_stream_round_trip() {
    let resource_name =
        std::env::var("VERTEX_REMOTE_ENGINE").expect("set VERTEX_REMOTE_ENGINE to run");
    let agent = RemoteReasoningEngineAgent::builder("live-remote")
        .resource_name(resource_name)
        .build()
        .await
        .expect("build remote agent");

    // The contract-test MockContext is crate-test-local; the live test uses
    // the same minimal shape.
    let events = live_support::run_and_collect(&agent).await;
    assert!(!events.is_empty(), "live engine returned no events");
    for event in &events {
        assert!(event.is_ok(), "live stream yielded an error: {event:?}");
    }
}

mod live_support {
    use adk_core::{
        Agent, CallbackContext, Content, Event, InvocationContext, ReadonlyContext, RunConfig,
        Session, State,
    };
    use futures::StreamExt;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct MockState;
    impl State for MockState {
        fn get(&self, _key: &str) -> Option<serde_json::Value> {
            None
        }
        fn set(&mut self, _key: String, _value: serde_json::Value) {}
        fn all(&self) -> HashMap<String, serde_json::Value> {
            HashMap::new()
        }
    }

    struct MockSession;
    impl Session for MockSession {
        fn id(&self) -> &str {
            "live-session"
        }
        fn app_name(&self) -> &str {
            "live-app"
        }
        fn user_id(&self) -> &str {
            "live-user"
        }
        fn state(&self) -> &dyn State {
            &MockState
        }
        fn conversation_history(&self) -> Vec<Content> {
            Vec::new()
        }
    }

    struct LiveContext {
        content: Content,
        config: RunConfig,
        session: MockSession,
    }

    #[async_trait::async_trait]
    impl ReadonlyContext for LiveContext {
        fn invocation_id(&self) -> &str {
            "live-invocation"
        }
        fn agent_name(&self) -> &str {
            "live-remote"
        }
        fn user_id(&self) -> &str {
            "live-user"
        }
        fn app_name(&self) -> &str {
            "live-app"
        }
        fn session_id(&self) -> &str {
            "live-session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait::async_trait]
    impl CallbackContext for LiveContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait::async_trait]
    impl InvocationContext for LiveContext {
        fn agent(&self) -> Arc<dyn Agent> {
            unimplemented!("not used by the remote agent")
        }
        fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
            None
        }
        fn session(&self) -> &dyn Session {
            &self.session
        }
        fn run_config(&self) -> &RunConfig {
            &self.config
        }
        fn end_invocation(&self) {}
        fn ended(&self) -> bool {
            false
        }
    }

    pub async fn run_and_collect(agent: &dyn Agent) -> Vec<adk_core::Result<Event>> {
        let ctx = Arc::new(LiveContext {
            content: Content::new("user").with_text("Reply with one short sentence."),
            config: RunConfig::default(),
            session: MockSession,
        });
        let stream = agent.run(ctx).await.expect("run");
        stream.collect().await
    }
}
