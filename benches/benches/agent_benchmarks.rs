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

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str { "bench-session" }
    fn app_name(&self) -> &str { "bench-app" }
    fn user_id(&self) -> &str { "bench-user" }
    fn state(&self) -> &dyn State { &MockState }
}

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> { None }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> { HashMap::new() }
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
    fn invocation_id(&self) -> &str { "bench-inv" }
    fn agent_name(&self) -> &str { "bench-agent" }
    fn user_id(&self) -> &str { "bench-user" }
    fn app_name(&self) -> &str { "bench-app" }
    fn session_id(&self) -> &str { "bench-session" }
    fn branch(&self) -> &str { "main" }
    fn user_content(&self) -> &Content { &self.user_content }
}

#[async_trait]
impl adk_core::CallbackContext for BenchContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> { None }
}

#[async_trait]
impl InvocationContext for BenchContext {
    fn agent(&self) -> Arc<dyn adk_core::Agent> { unimplemented!() }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> { None }
    fn session(&self) -> &dyn Session { &self.session }
    fn run_config(&self) -> &RunConfig { unimplemented!() }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool { false }
}

fn benchmark_simple_agent(c: &mut Criterion) {
    dotenv::dotenv().ok();
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
    
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    c.bench_function("agent_simple_execution", |b| {
        b.to_async(&rt).iter(|| async {
            let model = Arc::new(GeminiModel::new(api_key.clone(), "gemini-1.5-flash").unwrap());
            let agent = LlmAgentBuilder::new("bench-agent")
                .model(model)
                .build()
                .unwrap();

            let ctx = Arc::new(BenchContext::new(black_box("What is 2+2?")));
            let mut stream = agent.run(ctx).await.unwrap();
            
            while let Some(_) = stream.next().await {}
        });
    });
}

fn benchmark_multi_turn_agent(c: &mut Criterion) {
    dotenv::dotenv().ok();
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
    
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    c.bench_function("agent_multi_turn_execution", |b| {
        b.to_async(&rt).iter(|| async {
            let model = Arc::new(GeminiModel::new(api_key.clone(), "gemini-1.5-flash").unwrap());
            let agent = LlmAgentBuilder::new("bench-agent")
                .model(model)
                .instruction("You are a helpful math tutor. Answer concisely.")
                .build()
                .unwrap();

            // Simulate 3 turns
            for query in ["What is 2+2?", "What is 3+3?", "What is 4+4?"] {
                let ctx = Arc::new(BenchContext::new(black_box(query)));
                let mut stream = agent.run(ctx).await.unwrap();
                while let Some(_) = stream.next().await {}
            }
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10); // Reduce sample size for API calls
    targets = benchmark_simple_agent, benchmark_multi_turn_agent
}
criterion_main!(benches);
