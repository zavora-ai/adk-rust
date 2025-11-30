use adk_core::{
    inject_session_state, Artifacts, CallbackContext, Content, InvocationContext, Part,
    ReadonlyContext, RunConfig, Session, State,
};
use async_trait::async_trait;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

struct MockSession {
    state: MockState,
}

impl MockSession {
    fn new() -> Self {
        Self { state: MockState::new() }
    }
}

impl Session for MockSession {
    fn id(&self) -> &str {
        "template-bench-session"
    }
    fn app_name(&self) -> &str {
        "template-bench-app"
    }
    fn user_id(&self) -> &str {
        "template-bench-user"
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
}

struct MockState {
    data: HashMap<String, Value>,
}

impl MockState {
    fn new() -> Self {
        let mut data = HashMap::new();
        data.insert("user_name".to_string(), json!("Alice"));
        data.insert("role".to_string(), json!("admin"));
        data.insert("app:version".to_string(), json!("1.0.0"));
        data.insert("user:theme".to_string(), json!("dark"));
        Self { data }
    }
}

impl State for MockState {
    fn get(&self, key: &str) -> Option<Value> {
        self.data.get(key).cloned()
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        self.data.clone()
    }
}

struct BenchContext {
    session: MockSession,
    user_content: Content,
}

impl BenchContext {
    fn new() -> Self {
        Self {
            session: MockSession::new(),
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "test".to_string() }],
            },
        }
    }
}

#[async_trait]
impl ReadonlyContext for BenchContext {
    fn invocation_id(&self) -> &str {
        "template-bench-inv"
    }
    fn agent_name(&self) -> &str {
        "template-bench-agent"
    }
    fn user_id(&self) -> &str {
        "template-bench-user"
    }
    fn app_name(&self) -> &str {
        "template-bench-app"
    }
    fn session_id(&self) -> &str {
        "template-bench-session"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for BenchContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
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

fn benchmark_simple_template(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("template_simple_substitution", |b| {
        b.to_async(&rt).iter(|| async {
            let ctx = BenchContext::new();
            let template = black_box("Hello {user_name}, welcome back!");
            let _ = inject_session_state(&ctx, template).await.unwrap();
        });
    });
}

fn benchmark_complex_template(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("template_complex_substitution", |b| {
        b.to_async(&rt).iter(|| async {
            let ctx = BenchContext::new();
            let template = black_box(
                "User {user_name} with role {role} is using {app:version} with theme {user:theme}. \
                 Preferences: role={role}, theme={user:theme?}, unknown={unknown?}"
            );
            let _ = inject_session_state(&ctx, template).await.unwrap();
        });
    });
}

fn benchmark_template_with_many_placeholders(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("template_many_placeholders", |b| {
        b.to_async(&rt).iter(|| async {
            let ctx = BenchContext::new();
            let template = black_box(
                "{user_name} {user_name} {user_name} {role} {role} {role} \
                 {app:version} {app:version} {user:theme} {user:theme}",
            );
            let _ = inject_session_state(&ctx, template).await.unwrap();
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = benchmark_simple_template, benchmark_complex_template, benchmark_template_with_many_placeholders
}
criterion_main!(benches);
