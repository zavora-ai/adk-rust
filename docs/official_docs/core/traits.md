# Implementing the Core Traits

Every method of `Agent`, `Tool`, `Toolset` and `Llm`, with its default and when to override it.
[Core Types](core.md) introduces the traits with a first implementation of each; this page is the
reference for implementing one, wrapping one, and taking steering messages in a custom agent.

> **Important:** a type that wraps another and implements the same trait forwards every method
> it does not mean to change. A method it leaves out falls back to the trait default — not the
> wrapped value — and nothing fails to compile.

## Agent

| Method | Default | Override when |
|--------|---------|---------------|
| `name()` | Required | — unique within one agent tree |
| `description()` | Required | — shown to models when the agent is a transfer target or a tool |
| `sub_agents()` | Required | — return `&[]` for a leaf agent |
| `run(ctx)` | Required | — returns the `EventStream` of one invocation |
| `interaction_mode()` | `AgentInteractionMode::RequestResponse` | The agent holds a long-lived bidirectional session (`Realtime`); the server's agent details report it as a realtime agent |
| `supports_agent_transfer()` | `true` | The agent is a deterministic workflow whose sub-agents must not be resumed one by one; the runner then resumes the workflow root |
| `capabilities()` | `AgentCapabilities` with `handoff` from `supports_agent_transfer()`, `shared_state` and `invocation_metadata` | The agent consumes runtime tools, exact-call confirmation or checkpoint resume |
| `cancel_remote_task(task_id, acting)` | Does nothing | The agent starts tasks on another agent. The A2A v1 server calls it when a task it serves is cancelled while waiting on such a task; `RemoteA2aV1Agent` cancels the remote task |
| `profile()` | `AgentProfile` of kind `Custom` with nothing more | Callers that list the agent's skills, such as an A2A agent card, should see its kind, fixed instructions and tools; `LlmAgent` and the workflow agents describe themselves |
| `topology()` | `None` | The agent owns an explicit composition (members and relationships) |
| `configure_run(agent_name, config)` | Does nothing | A composite injects invocation-scoped tools or tightens limits before the runner builds the context |
| `transfer_targets_for(agent_name)` | `None` (parent and peer discovery) | The agent owns the exact handoff targets; `Some(vec![])` forbids handoff |
| `strict_transfer_policy()` | `false` | A transfer outside the policy should fail the run instead of being logged and refused |
| `govern_transfer(request)` | `AgentTransferDecision::Allow` | A valid transfer needs authorization, audit or another asynchronous check |
| `allows_transfer_to_parent()`, `allows_transfer_to_peers()` | `true` | The runner should refuse transfers to the parent, or to the parent's other sub-agents |
| `close()` | Closes each sub-agent | The agent owns toolsets, plugins or connections; close them, then the sub-agents. `Runner::close` calls it on the root |
| `prewarm()` | Prewarms each sub-agent | The agent owns a model or toolsets to ready before the first turn. `Runner::prewarm` calls it on the root |
| `screen_user_input(content)` | Screens with each sub-agent in order | The agent has input guardrails, or wraps an agent it does not list in `sub_agents()` |

An agent that wraps another without listing it in `sub_agents()` forwards `screen_user_input`,
`close`, `prewarm`, `profile`, `interaction_mode` and `cancel_remote_task` to it: the defaults of
the first three reach only listed sub-agents, so the wrapped agent's guardrails, connections and
warm-up are skipped otherwise.

### Taking steering messages in a custom agent

`Runner::steer` queues a user message on the run in flight (see
[Steering a Running Agent](runner.md#steering-a-running-agent)). `LlmAgent` and `CodeActAgent`
take the queue at their safe point; a custom agent takes it the same way:

1. At a point where the message can still change what the agent does next — after a step's
   result is in, before the next step starts — call `ctx.steering_inbox()`. It is `None` when the
   context gives the agent no steering, such as a `ParallelAgent` branch.
2. `inbox.drain()` returns the queued messages, oldest first.
3. Yield `message.to_event(ctx.invocation_id(), ctx.branch())` for each message, then use
   `message.content` in the agent's next step.

The runner saves each event — `author` `"user"`, with `EventActions::steering` set — and resolves
the sender's `SteerReceipt` to `SteerOutcome::Delivered`. A message still queued when the run ends
resolves to `SteerOutcome::Returned`. A context that wraps another forwards `steering_inbox()`.

The agent below works in three steps and takes steering between them. It needs the
`async-stream` crate:

```rust
use adk_core::{
    AdkIdentity, Agent, AppName, Content, Event, EventStream, InvocationContext, Part, Result,
    SessionId, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Works in three steps and takes steering messages between them.
struct StepAgent;

#[async_trait]
impl Agent for StepAgent {
    fn name(&self) -> &str {
        "stepper"
    }

    fn description(&self) -> &str {
        "Works in steps and takes steering between them"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let author = self.name().to_string();
        Ok(Box::pin(async_stream::stream! {
            let mut steering = Vec::new();
            for _step in 1..=3 {
                // One unit of work, such as a tool call.
                tokio::time::sleep(Duration::from_millis(200)).await;
                // The safe point: this step is done and the next has not started.
                if let Some(inbox) = ctx.steering_inbox() {
                    for message in inbox.drain() {
                        yield Ok(message.to_event(ctx.invocation_id(), ctx.branch()));
                        steering.extend(
                            message.content.parts.iter().filter_map(Part::text).map(str::to_string),
                        );
                    }
                }
            }
            let mut answer = Event::new(ctx.invocation_id());
            answer.author = author;
            answer.set_content(
                Content::new("model").with_text(format!("done; steering taken: {steering:?}")),
            );
            yield Ok(answer);
        }))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name("steer_demo")
            .agent(Arc::new(StepAgent))
            .session_service(sessions.clone())
            .build()?,
    );
    let session = sessions
        .create(CreateRequest {
            app_name: "steer_demo".to_string(),
            user_id: "dev".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let identity = AdkIdentity::new(
        AppName::try_from("steer_demo")?,
        UserId::try_from("dev")?,
        SessionId::try_from(session.id())?,
    );

    let mut events = runner
        .run(
            identity.user_id.clone(),
            identity.session_id.clone(),
            Content::new("user").with_text("run the three steps"),
        )
        .await?;

    // Steer the run while its first steps are going.
    let steer = {
        let runner = runner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let receipt = runner
                .steer(&identity, Content::new("user").with_text("skip the last step"))
                .await?;
            receipt.outcome().await
        })
    };

    while let Some(event) = events.next().await {
        let event = event?;
        let text: String = event
            .content()
            .map(|content| content.parts.iter().filter_map(Part::text).collect())
            .unwrap_or_default();
        println!("{}: {text} (steering: {})", event.author, event.actions.steering.is_some());
    }
    println!("{:?}", steer.await.expect("the steer task panicked")?);
    Ok(())
}
```

It prints the steering message as a `user` event with `steering: true`, the agent's answer naming
it, and `Delivered { event_id: … }`.

## Tool

| Method | Default | Override when |
|--------|---------|---------------|
| `name()`, `description()` | Required | — the function name and description the model sees |
| `execute(ctx, args)` | Required | — runs the call |
| `declaration()` | `name`, `enhanced_description()`, `parameters_schema()` and `response_schema()` as a function declaration | A provider built-in tool attaches metadata its adapter reads |
| `enhanced_description()` | `description()` | The model should read more than the description, such as a long-running tool's warning not to call it again |
| `is_long_running()` | `false` | The call starts work and returns before it finishes |
| `is_builtin()` | `false` | The provider runs the tool server-side; the agent does not execute it locally |
| `parameters_schema()`, `response_schema()` | `None` | The arguments, or the result, have a JSON Schema |
| `annotations()` | `None` | The publisher declares behaviour hints (`readOnlyHint`, `destructiveHint`, ...); MCP tools return the server's |
| `origin()` | `"static"` | The tool comes from a toolset or server; a held call's decision binds to it |
| `required_scopes()` | `&[]` | The caller must hold scopes to run the tool |
| `is_read_only()` | `false` | The tool has no side effects; with `is_concurrency_safe()`, `ToolExecutionStrategy::Auto` overlaps its calls |
| `effect()` | `ReadOnly` when `is_read_only()`, otherwise `NonIdempotent` | Repeating the call is safe (`Idempotent`); only `ReadOnly` and `Idempotent` calls are retried |
| `timeout_override()` | `None` (the agent's tool timeout) | The tool needs its own timeout, or none (`Some(None)`) |
| `is_concurrency_safe()` | `false` | Calls may run at the same time as other calls |
| `batch_group()` | `None` | Calls must run in the model's order and stop at the first that does not succeed; later calls in the group are answered with the group's `halt_text`. Computer use and browser use actions need it |
| `is_agent_delegation()` | `false` | The tool delegates to another agent; `parallelize_agent_delegations` overlaps consecutive delegation calls |
| `egress(args)` | `ToolEgress::Undeclared` | The call reaches the network. Under a run egress policy an undeclared call is refused before it runs |
| `approval_evidence(args)` | No evidence | An approver should see what the call would send, such as the exact image; the evidence digests are part of the call's fingerprint |

See [Function Tools](../tools/function-tools.md) for `FunctionTool` and `#[tool]`,
[Tool Effects](../tools/tool-effects.md) for `effect()`, and
[Tool Authorization](../security/tool-authorization.md) for origins, evidence and egress.

### Wrapping a tool

A wrapper forwards every method it does not change. A wrapper that leaves out `batch_group`
takes its tool out of the ordered batch, so a failed action no longer stops the actions the model
planned after it; one that leaves out `effect` makes the tool `NonIdempotent`, so it is never
retried; one that leaves out `egress` has its calls refused under an egress policy.

```rust
use adk_core::egress::ToolEgress;
use adk_core::{BatchGroup, Evidence, Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Logs each call, then runs the tool it wraps.
struct Logged(Arc<dyn Tool>);

#[async_trait]
impl Tool for Logged {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn description(&self) -> &str {
        self.0.description()
    }
    fn declaration(&self) -> Value {
        self.0.declaration()
    }
    fn enhanced_description(&self) -> String {
        self.0.enhanced_description()
    }
    fn is_long_running(&self) -> bool {
        self.0.is_long_running()
    }
    fn is_builtin(&self) -> bool {
        self.0.is_builtin()
    }
    fn parameters_schema(&self) -> Option<Value> {
        self.0.parameters_schema()
    }
    fn response_schema(&self) -> Option<Value> {
        self.0.response_schema()
    }
    fn annotations(&self) -> Option<&Value> {
        self.0.annotations()
    }
    fn origin(&self) -> &str {
        self.0.origin()
    }
    fn required_scopes(&self) -> &[&str] {
        self.0.required_scopes()
    }
    fn is_read_only(&self) -> bool {
        self.0.is_read_only()
    }
    fn effect(&self) -> ToolEffect {
        self.0.effect()
    }
    fn timeout_override(&self) -> Option<Option<Duration>> {
        self.0.timeout_override()
    }
    fn is_concurrency_safe(&self) -> bool {
        self.0.is_concurrency_safe()
    }
    fn batch_group(&self) -> Option<BatchGroup> {
        self.0.batch_group()
    }
    fn is_agent_delegation(&self) -> bool {
        self.0.is_agent_delegation()
    }
    fn egress(&self, args: &Value) -> ToolEgress {
        self.0.egress(args)
    }
    fn approval_evidence(&self, args: &Value) -> Vec<Evidence> {
        self.0.approval_evidence(args)
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        tracing::info!(tool.name = self.0.name(), "tool call");
        self.0.execute(ctx, args).await
    }
}
```

## Toolset

| Method | Default | Override when |
|--------|---------|---------------|
| `name()` | Required | — identifies the toolset |
| `tools(ctx)` | Required | — the tools for this context; `LlmAgent` resolves them at the start of each run and after each round of tool calls |
| `process_llm_request(ctx, request)` | Leaves the request unchanged | The toolset describes its own protocol to the model, as `SkillToolset` does |
| `close()` | Does nothing | The toolset holds a connection or a subprocess; `Agent::close` calls it, and closing twice must be harmless |

## Llm

| Method | Default | Override when |
|--------|---------|---------------|
| `name()` | Required | — the model identifier |
| `generate_content(request, stream)` | Required | — returns an `LlmResponseStream` |
| `generate_content_metered(scope, request, stream)` | Meters the call as one model call | The model sends one request to several providers, as `FallbackLlm` does, and meters each through `scope.generate` |
| `schema_adapter()` | `GenericSchemaAdapter` | The provider needs its own tool-schema normalization |
| `uses_interactions_api()` | `false` | The provider owns the tool loop and filesystem, which conflicts with client-side sandbox tools |
| `max_cost_micro_usd(request)` | `None` | The call's cost can be bounded; the spend ledger reserves this amount |
| `context_window()` | `None` | The model's token window is known; auto-compaction compacts before it |
| `native_web_search()` | `None` | The provider runs web search itself |
| `native_compaction()` | `None` | The provider compacts a conversation itself |
| `prewarm()` | Does nothing | The client can open its connection before the first call |
| `data_locality()` | `DataLocality::Remote` | The model runs in this process (`InProcess`) or on this machine (`Loopback`); the image privacy guardrail reads it |

A model wrapper forwards `max_cost_micro_usd`, `context_window`, `native_web_search`,
`native_compaction`, `prewarm` and `data_locality`; a wrapper over several models answers the
least local of their `data_locality` values (`DataLocality::least_local`). A wrapper that only
changes the request or the stream keeps the default `generate_content_metered`.

## Related

- [Core Types](core.md) — contexts, content, events and errors
- [Runner](runner.md) — closing, prewarming and steering runs
- [Function Tools](../tools/function-tools.md) — tools without implementing `Tool` by hand

---

**Previous**: [← Core Types](core.md) | **Next**: [Runner →](runner.md)
