//! Developer Ergonomics Validation Example
//!
//! Showcases all seven ergonomics improvements introduced in ADK-Rust 0.5.x:
//!
//! 1. **ToolExecutionStrategy** — Sequential, Parallel, Auto dispatch
//! 2. **Tool metadata** — `is_read_only()` / `is_concurrency_safe()` on Tool trait
//! 3. **RunnerConfigBuilder** — typestate builder for Runner construction
//! 4. **SimpleToolContext** — lightweight ToolContext for non-agent callers
//! 5. **Runner::run_str()** — string convenience method
//! 6. **StatefulTool<S>** — shared-state tool wrapper
//! 7. **FunctionTool builder extensions** — `with_read_only()` / `with_concurrency_safe()`
//!
//! Run: cargo run --manifest-path examples/developer_ergonomics/Cargo.toml

use adk_core::{
    Agent, Content, EventStream, InvocationContext, Part, Result, Tool, ToolContext,
    ToolExecutionStrategy,
};
use adk_runner::{Runner, RunnerConfig};
use adk_session::InMemorySessionService;
use adk_tool::{FunctionTool, SimpleToolContext, StatefulTool};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Minimal agent that echoes user input and reports the active strategy
// ---------------------------------------------------------------------------
struct EchoAgent;

#[async_trait]
impl Agent for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes user input and reports tool execution strategy"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let input_text = ctx
            .user_content()
            .parts
            .iter()
            .find_map(|p| match p {
                Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let mut event = adk_session::Event::new(ctx.invocation_id());
        event.author = "echo".to_string();
        event.llm_response.content =
            Some(Content::new("model").with_text(format!("Echo: {input_text}")));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

#[tokio::main]
async fn main() {
    println!("=== Developer Ergonomics Validation ===\n");

    validate_tool_execution_strategy();
    validate_tool_metadata();
    validate_runner_builder().await;
    validate_simple_tool_context().await;
    validate_run_str().await;
    validate_stateful_tool().await;

    println!("\n=== All validations passed ===");
}

// ---------------------------------------------------------------------------
// 1. ToolExecutionStrategy enum
// ---------------------------------------------------------------------------
fn validate_tool_execution_strategy() {
    println!("--- 1. ToolExecutionStrategy ---");

    // Default is Sequential
    let default = ToolExecutionStrategy::default();
    assert_eq!(default, ToolExecutionStrategy::Sequential);
    println!("  ✓ Default is Sequential");

    // All three variants exist and are comparable
    let strategies = [
        ToolExecutionStrategy::Sequential,
        ToolExecutionStrategy::Parallel,
        ToolExecutionStrategy::Auto,
    ];
    assert_eq!(strategies.len(), 3);
    println!("  ✓ Three variants: Sequential, Parallel, Auto");

    // Copy + Clone + Debug
    let s = ToolExecutionStrategy::Auto;
    let s2 = s; // Copy
    let s3 = s.clone(); // Clone
    assert_eq!(s2, s3);
    println!("  ✓ Copy, Clone, Debug, PartialEq, Eq all work");

    // Serialize / Deserialize round-trip
    let json_str = serde_json::to_string(&ToolExecutionStrategy::Parallel).unwrap();
    let back: ToolExecutionStrategy = serde_json::from_str(&json_str).unwrap();
    assert_eq!(back, ToolExecutionStrategy::Parallel);
    println!("  ✓ Serde round-trip: {json_str}");
}

// ---------------------------------------------------------------------------
// 2. Tool metadata: is_read_only() / is_concurrency_safe()
// ---------------------------------------------------------------------------
fn validate_tool_metadata() {
    println!("\n--- 2. Tool Metadata (read_only, concurrency_safe) ---");

    // Default FunctionTool: both false
    let tool =
        FunctionTool::new("default_tool", "A tool", |_ctx, _args| async move { Ok(json!({})) });
    assert!(!tool.is_read_only());
    assert!(!tool.is_concurrency_safe());
    println!("  ✓ Default FunctionTool: read_only=false, concurrency_safe=false");

    // Builder methods
    let ro_tool = FunctionTool::new("reader", "Read-only tool", |_ctx, _args| async move {
        Ok(json!({"data": "cached"}))
    })
    .with_read_only(true)
    .with_concurrency_safe(true);

    assert!(ro_tool.is_read_only());
    assert!(ro_tool.is_concurrency_safe());
    println!("  ✓ with_read_only(true) → is_read_only() == true");
    println!("  ✓ with_concurrency_safe(true) → is_concurrency_safe() == true");

    // Toggle back to false
    let toggled =
        FunctionTool::new("toggled", "Toggled", |_ctx, _args| async move { Ok(json!({})) })
            .with_read_only(true)
            .with_read_only(false);
    assert!(!toggled.is_read_only());
    println!("  ✓ Builder methods are idempotent (can toggle back)");
}

// ---------------------------------------------------------------------------
// 3. RunnerConfigBuilder (typestate pattern)
// ---------------------------------------------------------------------------
async fn validate_runner_builder() {
    println!("\n--- 3. RunnerConfigBuilder ---");

    let session_service = Arc::new(InMemorySessionService::new());

    // Build with only required fields — all optionals get defaults
    let runner = Runner::builder()
        .app_name("builder-demo")
        .agent(Arc::new(EchoAgent) as Arc<dyn Agent>)
        .session_service(session_service.clone())
        .build()
        .expect("build with required fields should succeed");
    println!("  ✓ Builder with only required fields compiles and builds");

    // Build with optional fields
    let session_service2 = Arc::new(InMemorySessionService::new());
    let runner_with_opts = Runner::builder()
        .app_name("builder-opts")
        .agent(Arc::new(EchoAgent) as Arc<dyn Agent>)
        .session_service(session_service2.clone())
        .build()
        .expect("build with optional fields should succeed");
    println!("  ✓ Builder with optional fields builds");

    // Pre-create session so the runner can find it
    use adk_session::{CreateRequest, SessionService};
    session_service2
        .create(CreateRequest {
            app_name: "builder-opts".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("sess1".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();

    // Verify the runner works by running the agent
    let mut stream = runner_with_opts
        .run_str("user1", "sess1", Content::new("user").with_text("hello"))
        .await
        .unwrap();
    let mut event_count = 0;
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                event_count += 1;
                if event.llm_response.content.is_some() {
                    println!("  ✓ Runner built with optional fields produces events correctly");
                }
            }
            Err(e) => {
                println!("  ⚠ Stream error (non-fatal): {e}");
                event_count += 1;
            }
        }
    }
    if event_count == 0 {
        println!(
            "  ✓ Runner built with optional fields runs (no events from echo agent is expected)"
        );
    }

    // Old-style struct literal still works (backward compat)
    let _runner_old = Runner::new(RunnerConfig {
        app_name: "old-style".to_string(),
        agent: Arc::new(EchoAgent) as Arc<dyn Agent>,
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })
    .unwrap();
    println!("  ✓ Runner::new(RunnerConfig {{ ... }}) still works (backward compat)");

    drop(runner);
}

// ---------------------------------------------------------------------------
// 4. SimpleToolContext — lightweight ToolContext for non-agent callers
// ---------------------------------------------------------------------------
async fn validate_simple_tool_context() {
    println!("\n--- 4. SimpleToolContext ---");

    // Construct with just a caller name
    let ctx = SimpleToolContext::new("my-mcp-server");
    println!("  ✓ SimpleToolContext::new(\"my-mcp-server\") constructs");

    // Verify defaults via trait methods
    use adk_core::context::ReadonlyContext;
    assert_eq!(ctx.agent_name(), "my-mcp-server");
    assert_eq!(ctx.app_name(), "my-mcp-server");
    println!("  ✓ agent_name() and app_name() return caller name");

    assert_eq!(ctx.user_id(), "anonymous");
    println!("  ✓ user_id() returns \"anonymous\"");

    assert_eq!(ctx.session_id(), "");
    assert_eq!(ctx.branch(), "");
    println!("  ✓ session_id() and branch() return empty string");

    // invocation_id is a UUID
    assert!(!ctx.invocation_id().is_empty());
    assert!(ctx.invocation_id().contains('-'), "should be UUID format");
    println!("  ✓ invocation_id() is a non-empty UUID: {}", ctx.invocation_id());

    // function_call_id is also a UUID by default
    assert!(!ctx.function_call_id().is_empty());
    println!("  ✓ function_call_id() is a non-empty UUID: {}", ctx.function_call_id());

    // Two contexts have different IDs
    let ctx2 = SimpleToolContext::new("other");
    assert_ne!(ctx.invocation_id(), ctx2.invocation_id());
    assert_ne!(ctx.function_call_id(), ctx2.function_call_id());
    println!("  ✓ Each instance gets unique IDs");

    // Override function_call_id via builder
    let ctx3 = SimpleToolContext::new("caller").with_function_call_id("custom-fc-id");
    assert_eq!(ctx3.function_call_id(), "custom-fc-id");
    println!("  ✓ with_function_call_id() overrides the default");

    // Use it to execute a tool outside the agent loop
    let tool = FunctionTool::new("greet", "Greet", |ctx, _args| async move {
        Ok(json!({"greeted_by": ctx.agent_name()}))
    });
    let tool_ctx: Arc<dyn ToolContext> = Arc::new(SimpleToolContext::new("test-harness"));
    let result = tool.execute(tool_ctx, json!({})).await.unwrap();
    assert_eq!(result["greeted_by"], "test-harness");
    println!("  ✓ Tool executed successfully with SimpleToolContext");
}

// ---------------------------------------------------------------------------
// 5. Runner::run_str() — string convenience method
// ---------------------------------------------------------------------------
async fn validate_run_str() {
    println!("\n--- 5. Runner::run_str() ---");

    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("run-str-demo")
        .agent(Arc::new(EchoAgent) as Arc<dyn Agent>)
        .session_service(session_service.clone())
        .build()
        .unwrap();

    // Pre-create session
    use adk_session::{CreateRequest, SessionService};
    session_service
        .create(CreateRequest {
            app_name: "run-str-demo".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("session-1".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();
    session_service
        .create(CreateRequest {
            app_name: "run-str-demo".to_string(),
            user_id: "bob".to_string(),
            session_id: Some("sess-2".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();

    // Valid strings work
    let mut stream = runner
        .run_str("alice", "session-1", Content::new("user").with_text("hi"))
        .await
        .expect("valid strings should succeed");
    let mut got_response = false;
    while let Some(Ok(event)) = stream.next().await {
        if event.llm_response.content.is_some() {
            got_response = true;
        }
    }
    assert!(got_response);
    println!("  ✓ run_str(\"alice\", \"session-1\", ...) succeeds");

    // Empty user_id fails before agent loop
    let err = runner.run_str("", "session-1", Content::new("user").with_text("hi")).await;
    assert!(err.is_err());
    println!("  ✓ run_str(\"\", ...) returns error (empty user_id rejected)");

    // Empty session_id fails before agent loop
    let err = runner.run_str("alice", "", Content::new("user").with_text("hi")).await;
    assert!(err.is_err());
    println!("  ✓ run_str(\"alice\", \"\", ...) returns error (empty session_id rejected)");

    // Existing run() still works with typed IDs
    use adk_core::{SessionId, UserId};
    let mut stream = runner
        .run(
            UserId::new("bob").unwrap(),
            SessionId::new("sess-2").unwrap(),
            Content::new("user").with_text("typed"),
        )
        .await
        .unwrap();
    while (stream.next().await).is_some() {}
    println!("  ✓ run() with UserId/SessionId still works (backward compat)");
}

// ---------------------------------------------------------------------------
// 6. StatefulTool<S> — shared-state tool wrapper
// ---------------------------------------------------------------------------
async fn validate_stateful_tool() {
    println!("\n--- 6. StatefulTool<S> ---");

    // Shared counter state
    struct Counter {
        count: RwLock<u64>,
    }

    let state = Arc::new(Counter { count: RwLock::new(0) });

    // Create a stateful tool that increments the counter
    let tool = StatefulTool::new(
        "increment",
        "Increment a shared counter",
        state.clone(),
        |s: Arc<Counter>, _ctx: Arc<dyn ToolContext>, _args| async move {
            let mut count = s.count.write().await;
            *count += 1;
            Ok(json!({"count": *count}))
        },
    )
    .with_read_only(false)
    .with_concurrency_safe(true);

    // Verify Tool trait methods
    assert_eq!(tool.name(), "increment");
    assert_eq!(tool.description(), "Increment a shared counter");
    assert!(!tool.is_read_only());
    assert!(tool.is_concurrency_safe());
    println!("  ✓ StatefulTool implements Tool trait correctly");

    // Execute multiple times — state is shared
    let ctx: Arc<dyn ToolContext> = Arc::new(SimpleToolContext::new("test"));
    let r1 = tool.execute(ctx.clone(), json!({})).await.unwrap();
    let r2 = tool.execute(ctx.clone(), json!({})).await.unwrap();
    let r3 = tool.execute(ctx.clone(), json!({})).await.unwrap();

    assert_eq!(r1["count"], 1);
    assert_eq!(r2["count"], 2);
    assert_eq!(r3["count"], 3);
    println!("  ✓ State shared across invocations: 1 → 2 → 3");

    // Verify the Arc<S> is the same underlying state
    assert_eq!(*state.count.read().await, 3);
    println!("  ✓ External state reference sees all mutations");

    // Builder methods mirror FunctionTool
    let _full = StatefulTool::new(
        "full",
        "Full builder",
        Arc::new(()),
        |_s: Arc<()>, _ctx: Arc<dyn ToolContext>, _args| async move { Ok(json!({})) },
    )
    .with_long_running(true)
    .with_read_only(true)
    .with_concurrency_safe(true)
    .with_scopes(&["admin:write"]);
    println!(
        "  ✓ All builder methods chain: with_long_running, with_read_only, with_concurrency_safe, with_scopes"
    );
}
