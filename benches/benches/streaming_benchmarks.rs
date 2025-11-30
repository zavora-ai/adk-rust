use adk_agent::LlmAgentBuilder;
use adk_core::{Content, InvocationContext, Part, ReadonlyContext, RunConfig, Session, State};
use adk_model::GeminiModel;
use async_trait::async_trait;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Instant;

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "stream-bench-session"
    }
    fn app_name(&self) -> &str {
        "stream-bench-app"
    }
    fn user_id(&self) -> &str {
        "stream-bench-user"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
}

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct BenchContext {
    session: MockSession,
    user_content: Content,
}

impl BenchContext {
    fn new(text: &str) -> Self {
        Self {
            session: MockSession,
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: text.to_string() }],
            },
        }
    }
}

#[async_trait]
impl ReadonlyContext for BenchContext {
    fn invocation_id(&self) -> &str {
        "stream-bench-inv"
    }
    fn agent_name(&self) -> &str {
        "stream-bench-agent"
    }
    fn user_id(&self) -> &str {
        "stream-bench-user"
    }
    fn app_name(&self) -> &str {
        "stream-bench-app"
    }
    fn session_id(&self) -> &str {
        "stream-bench-session"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for BenchContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for BenchContext {
    fn agent(&self) -> Arc<dyn adk_core::Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        unimplemented!()
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

fn benchmark_time_to_first_token(c: &mut Criterion) {
    dotenv::dotenv().ok();
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");

    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("streaming_time_to_first_token", |b| {
        b.to_async(&rt).iter(|| async {
            let model = Arc::new(GeminiModel::new(api_key.clone(), "gemini-1.5-flash").unwrap());
            let agent = LlmAgentBuilder::new("stream-bench-agent").model(model).build().unwrap();

            let ctx = Arc::new(BenchContext::new(black_box("Write a short story about a robot.")));
            let mut stream = agent.run(ctx).await.unwrap();

            let start = Instant::now();
            // Get first token
            if let Some(_) = stream.next().await {
                let _ttft = start.elapsed();
            }

            // Consume rest
            while let Some(_) = stream.next().await {}
        });
    });
}

fn benchmark_streaming_throughput(c: &mut Criterion) {
    dotenv::dotenv().ok();
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");

    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("streaming_full_throughput", |b| {
        b.to_async(&rt).iter(|| async {
            let model = Arc::new(GeminiModel::new(api_key.clone(), "gemini-1.5-flash").unwrap());
            let agent = LlmAgentBuilder::new("stream-bench-agent").model(model).build().unwrap();

            let ctx = Arc::new(BenchContext::new(black_box(
                "Explain quantum computing in 2 paragraphs.",
            )));
            let mut stream = agent.run(ctx).await.unwrap();

            let start = Instant::now();
            let mut token_count = 0;

            while let Some(result) = stream.next().await {
                if let Ok(event) = result {
                    if let Some(content) = event.llm_response.content {
                        for part in content.parts {
                            if let Part::Text { text } = part {
                                token_count += text.split_whitespace().count();
                            }
                        }
                    }
                }
            }

            let elapsed = start.elapsed();
            let _throughput = token_count as f64 / elapsed.as_secs_f64();
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = benchmark_time_to_first_token, benchmark_streaming_throughput
}
criterion_main!(benches);
