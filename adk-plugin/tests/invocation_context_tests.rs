//! Each invocation's hooks get their own `PluginContext` (spec 3.1 §12 phase 0).
//!
//! One `EnhancedPluginManager` serves every run of its agent, so a context shared by the
//! whole manager let one user's run read and overwrite another's plugin state.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use adk_core::{
    CallbackContext, Content, LlmRequest, ReadonlyContext, Result, Tool, ToolContext, async_trait,
};
use adk_plugin::{
    AfterToolCallResult, BeforeModelCallResult, BeforeToolCallResult, EnhancedPlugin,
    EnhancedPluginManager, PluginContext,
};
use serde_json::{Value, json};

struct Ctx {
    user: String,
    invocation: String,
    content: Content,
}

fn ctx(user: &str, invocation: &str) -> Arc<dyn CallbackContext> {
    Arc::new(Ctx {
        user: user.to_string(),
        invocation: invocation.to_string(),
        content: Content::new("user"),
    })
}

#[async_trait]
impl ReadonlyContext for Ctx {
    fn invocation_id(&self) -> &str {
        &self.invocation
    }
    fn agent_name(&self) -> &str {
        "assistant"
    }
    fn user_id(&self) -> &str {
        &self.user
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn session_id(&self) -> &str {
        "shared-session-name"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for Ctx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

struct Lookup;

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }
    fn description(&self) -> &str {
        "Looks something up"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(Value::Null)
    }
}

/// Calls made in this invocation.
#[derive(Clone, Debug, PartialEq)]
struct Calls(u32);

/// Calls made by every invocation of the agent. The test installs the counter in the
/// manager's context before the runs: `get` and `insert` are two steps, so two invocations
/// counting with them at once would lose calls.
#[derive(Clone, Debug)]
struct AgentCalls(Arc<AtomicU32>);

/// The user whose run wrote the invocation's state.
#[derive(Clone, Debug, PartialEq)]
struct Owner(String);

/// Counts calls per invocation and per agent, and reports what it saw in the arguments.
struct Counting;

#[async_trait]
impl EnhancedPlugin for Counting {
    fn name(&self) -> &str {
        "counting"
    }

    async fn before_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        mut args: Value,
        ctx: Arc<dyn CallbackContext>,
        plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        let calls = plugin_ctx.get::<Calls>().await.map_or(1, |Calls(n)| n + 1);
        // Let the other invocation run between the read and the write.
        tokio::task::yield_now().await;
        plugin_ctx.insert(Calls(calls)).await;
        if plugin_ctx.get::<Owner>().await.is_none() {
            plugin_ctx.insert(Owner(ctx.user_id().to_string())).await;
        }
        if let Some(AgentCalls(total)) = plugin_ctx.shared().get::<AgentCalls>().await {
            total.fetch_add(1, Ordering::SeqCst);
        }

        args["calls"] = json!(calls);
        args["owner"] = json!(plugin_ctx.get::<Owner>().await.map(|Owner(user)| user));
        Ok(BeforeToolCallResult::Continue(args))
    }

    async fn after_tool_call(
        &self,
        _tool: Arc<dyn Tool>,
        _args: &Value,
        _result: Value,
        _ctx: Arc<dyn CallbackContext>,
        plugin_ctx: &PluginContext,
    ) -> Result<AfterToolCallResult> {
        let calls = plugin_ctx.get::<Calls>().await.map(|Calls(n)| n);
        Ok(AfterToolCallResult::Continue(json!({ "callsSeenAfter": calls })))
    }

    async fn before_model_call(
        &self,
        mut request: LlmRequest,
        _ctx: Arc<dyn CallbackContext>,
        plugin_ctx: &PluginContext,
    ) -> Result<BeforeModelCallResult> {
        let owner = plugin_ctx.get::<Owner>().await.map(|Owner(user)| user);
        request.model = format!("model-for-{}", owner.as_deref().unwrap_or("nobody"));
        Ok(BeforeModelCallResult::Continue(request))
    }
}

async fn call(manager: &EnhancedPluginManager, ctx: Arc<dyn CallbackContext>) -> Value {
    match manager.run_before_tool_call(Arc::new(Lookup), json!({}), ctx).await.unwrap() {
        BeforeToolCallResult::Continue(args) => args,
        BeforeToolCallResult::ShortCircuit(_) => panic!("the counting plugin never answers"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_invocations_never_share_plugin_state() {
    const CALLS: u32 = 25;
    let manager = Arc::new(EnhancedPluginManager::new(vec![Arc::new(Counting)]));
    let total = Arc::new(AtomicU32::new(0));
    manager.context().insert(AgentCalls(Arc::clone(&total))).await;

    let runs: Vec<_> = [("alice", "inv-alice"), ("bob", "inv-bob")]
        .into_iter()
        .map(|(user, invocation)| {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move {
                let mut last = Value::Null;
                for _ in 0..CALLS {
                    last = call(&manager, ctx(user, invocation)).await;
                }
                last
            })
        })
        .collect();
    let mut seen = Vec::new();
    for run in runs {
        seen.push(run.await.unwrap());
    }

    assert_eq!(
        seen,
        vec![
            json!({ "calls": CALLS, "owner": "alice" }),
            json!({ "calls": CALLS, "owner": "bob" }),
        ],
        "each invocation counts only its own calls and keeps its own owner"
    );
    assert_eq!(total.load(Ordering::SeqCst), 2 * CALLS, "the shared context sees every invocation");
}

#[tokio::test]
async fn every_hook_of_one_invocation_gets_the_same_context() {
    let manager = EnhancedPluginManager::new(vec![Arc::new(Counting)]);
    call(&manager, ctx("alice", "inv-1")).await;
    call(&manager, ctx("alice", "inv-1")).await;

    let after = manager
        .run_after_tool_call(Arc::new(Lookup), &json!({}), Value::Null, ctx("alice", "inv-1"))
        .await
        .unwrap();
    let AfterToolCallResult::Continue(after) = after;
    assert_eq!(after, json!({ "callsSeenAfter": 2 }));

    let model = manager
        .run_before_model_call(LlmRequest::new("base", vec![]), ctx("alice", "inv-1"))
        .await
        .unwrap();
    let BeforeModelCallResult::Continue(request) = model else {
        panic!("the counting plugin never answers");
    };
    assert_eq!(request.model, "model-for-alice");

    let fresh = manager
        .run_before_model_call(LlmRequest::new("base", vec![]), ctx("alice", "inv-2"))
        .await
        .unwrap();
    let BeforeModelCallResult::Continue(request) = fresh else {
        panic!("the counting plugin never answers");
    };
    assert_eq!(request.model, "model-for-nobody", "a new invocation starts empty");
}

#[tokio::test]
async fn an_invocation_id_reused_by_another_user_gets_a_separate_context() {
    let manager = EnhancedPluginManager::new(vec![Arc::new(Counting)]);
    call(&manager, ctx("alice", "inv-1")).await;
    assert_eq!(call(&manager, ctx("bob", "inv-1")).await, json!({ "calls": 1, "owner": "bob" }));

    let alice = manager.invocation_context(ctx("alice", "inv-1").as_ref());
    let bob = manager.invocation_context(ctx("bob", "inv-1").as_ref());
    assert!(!Arc::ptr_eq(&alice, &bob));
    assert!(std::ptr::eq(alice.shared(), manager.context().as_ref()));
}
