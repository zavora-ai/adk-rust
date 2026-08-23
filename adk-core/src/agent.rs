use crate::{InvocationContext, Result, RunConfig, event::Event};
use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;

/// A pinned, boxed stream of [`Event`] results emitted by an agent during execution.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>;

/// Runtime composition features an [`Agent`] can consume or enforce.
///
/// The declaration is intentionally separate from an agent's domain-specific
/// skills. It lets portable composition validate that a coordinator can accept
/// invocation-scoped tools, emit governed transfers, and participate in safe
/// resume before execution starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// The agent consumes [`RunConfig::runtime_toolsets`].
    pub runtime_tools: bool,
    /// The agent can emit agent-to-agent transfer actions.
    pub handoff: bool,
    /// Runtime-injected tools can use exact-call confirmation policy.
    pub relationship_confirmation: bool,
    /// The agent can safely resume an unresolved operation from a durable checkpoint.
    pub checkpoint_resume: bool,
    /// The agent observes invocation shared state when it is supplied.
    pub shared_state: bool,
    /// The agent forwards cancellation and authenticated request metadata.
    pub invocation_metadata: bool,
}

/// Primary interaction pattern exposed by an [`Agent`].
///
/// This describes how a runtime should present the agent, not how the agent is
/// composed. Teams and workflows therefore retain their existing agent
/// primitives while a realtime implementation can advertise audio-oriented
/// interaction to generic servers and user interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AgentInteractionMode {
    /// A bounded request produces a bounded stream of response events.
    #[default]
    RequestResponse,
    /// A long-lived bidirectional session may emit audio and transcript events.
    Realtime,
}

/// Portable description of a member in an agent composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTopologyMember {
    /// Stable member name used by the runtime.
    pub name: String,
    /// Human-readable member purpose.
    pub description: String,
    /// Whether this member receives the initial request.
    pub coordinator: bool,
    /// Runtime capabilities declared by the bound member.
    pub capabilities: AgentCapabilities,
}

/// Control-flow semantics for one portable composition relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentRelationshipKind {
    /// Deterministic workflow control flows from the source node to the target node.
    Flow,
    /// Invoke the target and return its result to the caller.
    Delegate,
    /// Transfer active control to the target.
    Handoff,
}

/// One exact directed relationship in a portable agent composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTopologyRelationship {
    /// Calling or transferring member.
    pub from: String,
    /// Exact target member.
    pub to: String,
    /// Relationship execution semantics.
    pub kind: AgentRelationshipKind,
}

/// Portable topology metadata for a composed agent root.
///
/// The metadata is deliberately execution-provider neutral. Runtimes and user
/// interfaces can inspect a composition without depending on a concrete team,
/// graph, workflow, or model implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTopology {
    /// Stable executable root name.
    pub root: String,
    /// Member that receives the initial request.
    pub coordinator: String,
    /// Members bound to the composition.
    pub members: Vec<AgentTopologyMember>,
    /// Exact directed relationships between members.
    pub relationships: Vec<AgentTopologyRelationship>,
}

/// One proposed agent-to-agent transfer presented to a composite root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTransferRequest {
    /// Root invocation that owns the transfer chain.
    pub invocation_id: String,
    /// Member requesting the transfer.
    pub from: String,
    /// Proposed target member.
    pub to: String,
    /// One-based transfer depth if the transfer is accepted.
    pub depth: u32,
}

/// Result of asynchronous transfer governance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "decision")]
pub enum AgentTransferDecision {
    /// Accept the exact proposed target.
    Allow,
    /// Reject the transfer with an auditable reason.
    Deny {
        /// Human-readable policy reason.
        reason: String,
    },
}

/// The fundamental trait for all ADK agents.
///
/// Every agent — whether a simple LLM wrapper, a multi-step workflow, or a
/// composite orchestrator — implements this trait. The runtime invokes
/// [`run`](Self::run) with an [`InvocationContext`] and consumes the returned
/// [`EventStream`].
#[async_trait]
pub trait Agent: Send + Sync {
    /// Returns the unique name of this agent.
    fn name(&self) -> &str;
    /// Returns a human-readable description of this agent's purpose.
    fn description(&self) -> &str;
    /// Returns the child agents managed by this agent.
    fn sub_agents(&self) -> &[Arc<dyn Agent>];

    /// Returns the agent's primary interaction pattern.
    ///
    /// Existing agents remain request/response by default. Realtime agents
    /// override this without introducing a new atomic agent kind or changing
    /// composition semantics.
    fn interaction_mode(&self) -> AgentInteractionMode {
        AgentInteractionMode::RequestResponse
    }

    /// Whether this agent participates in LLM-driven agent transfer and may be
    /// resumed directly across conversation turns.
    ///
    /// When a session persists across turns, the runner inspects history to
    /// decide which agent should handle the next user message. LLM-based and
    /// custom agents return the default `true`, so the runner can hand a new
    /// turn back to whichever agent responded last.
    ///
    /// Deterministic workflow agents (sequential, parallel, loop, conditional)
    /// override this to return `false`. Their sub-agents must not be resumed
    /// individually: doing so would skip the workflow's other sub-agents on
    /// subsequent turns. Returning `false` makes the runner resume the workflow
    /// root instead, so every sub-agent runs again on each turn.
    fn supports_agent_transfer(&self) -> bool {
        true
    }

    /// Declares the execution-plane capabilities this agent supports.
    ///
    /// The default remains compatible with existing custom agents: transfer,
    /// shared-state, cancellation, and request metadata follow the established
    /// [`Agent`] and [`InvocationContext`] contracts. Runtime tool injection and
    /// exact-call relationship confirmation are opt-in because an implementation
    /// must actively consume those facilities.
    fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities {
            runtime_tools: false,
            handoff: self.supports_agent_transfer(),
            relationship_confirmation: false,
            checkpoint_resume: false,
            shared_state: true,
            invocation_metadata: true,
        }
    }

    /// Returns portable composition metadata when this agent owns an explicit topology.
    ///
    /// Leaf agents and legacy composites return `None`. The default keeps this
    /// additive API backward compatible for existing [`Agent`] implementations.
    fn topology(&self) -> Option<AgentTopology> {
        None
    }

    /// Applies agent-composition policy to a run before the runtime creates the
    /// invocation context for `agent_name`.
    ///
    /// Composite agents can use this hook to inject invocation-scoped tools,
    /// constrain transfer targets, or tighten depth and concurrency limits.
    /// The default is a no-op, preserving the behavior of existing agents.
    fn configure_run(&self, _agent_name: &str, _config: &mut RunConfig) {}

    /// Returns the exact handoff targets allowed for `agent_name`, when this
    /// agent owns an explicit transfer topology.
    ///
    /// `None` asks the runtime to retain its legacy parent/peer discovery.
    /// `Some(vec![])` explicitly forbids handoff from the named agent.
    fn transfer_targets_for(&self, _agent_name: &str) -> Option<Vec<String>> {
        None
    }

    /// Whether transfer-policy violations should fail the run.
    ///
    /// Legacy agent trees return `false`, so missing targets and exceeded depth
    /// retain their historical warn-and-stop behavior. Validated composites
    /// return `true` to make topology violations observable errors.
    fn strict_transfer_policy(&self) -> bool {
        false
    }

    /// Applies asynchronous policy to an otherwise valid transfer.
    ///
    /// Runner calls this after exact target validation and before control moves
    /// to the target. Ordinary agents allow transfers, preserving legacy
    /// behavior; validated composites can attach authorization, lifecycle hooks,
    /// audit logging, or other policy without teaching Runner their schema.
    async fn govern_transfer(
        &self,
        _request: &AgentTransferRequest,
    ) -> Result<AgentTransferDecision> {
        Ok(AgentTransferDecision::Allow)
    }

    /// Executes the agent and returns a stream of events.
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream>;
}

/// A validated context containing engineered instructions and resolved tool instances.
///
/// This structure serves as the "Atomic Unit of Capability" for an agent. It guarantees
/// that the agent's cognitive frame (the instructions telling it what it can do) is
/// perfectly aligned with its physical capabilities (the binary tool instances bound
/// to the session).
///
/// By using `ResolvedContext`, the framework eliminates "Phantom Tool" hallucinations,
/// where an agent tries to call a tool that was mentioned in its prompt but never
/// actually registered in the runtime.
#[derive(Clone)]
pub struct ResolvedContext {
    /// The engineered system instruction.
    pub system_instruction: String,
    /// The resolved, executable tools.
    pub active_tools: Vec<Arc<dyn crate::Tool>>,
}

impl std::fmt::Debug for ResolvedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedContext")
            .field("system_instruction_len", &self.system_instruction.len())
            .field("active_tools_count", &self.active_tools.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, ReadonlyContext, RunConfig};
    use async_stream::stream;

    struct TestAgent {
        name: String,
    }

    use crate::{CallbackContext, Session, State};
    use std::collections::HashMap;

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
            "session"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn state(&self) -> &dyn State {
            &MockState
        }
        fn conversation_history(&self) -> Vec<Content> {
            Vec::new()
        }
    }

    #[allow(dead_code)]
    struct TestContext {
        content: Content,
        config: RunConfig,
        session: MockSession,
    }

    #[allow(dead_code)]
    impl TestContext {
        fn new() -> Self {
            Self {
                content: Content::new("user"),
                config: RunConfig::default(),
                session: MockSession,
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestContext {
        fn invocation_id(&self) -> &str {
            "test"
        }
        fn agent_name(&self) -> &str {
            "test"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            "session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for TestContext {
        fn artifacts(&self) -> Option<Arc<dyn crate::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl InvocationContext for TestContext {
        fn agent(&self) -> Arc<dyn Agent> {
            unimplemented!()
        }
        fn memory(&self) -> Option<Arc<dyn crate::Memory>> {
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

    #[async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "test agent"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            let s = stream! {
                yield Ok(Event::new("test"));
            };
            Ok(Box::pin(s))
        }
    }

    #[test]
    fn test_agent_trait() {
        let agent = TestAgent { name: "test".to_string() };
        assert_eq!(agent.name(), "test");
        assert_eq!(agent.description(), "test agent");
        assert!(agent.capabilities().handoff);
        assert!(!agent.capabilities().runtime_tools);
        assert_eq!(agent.interaction_mode(), AgentInteractionMode::RequestResponse);
        assert_eq!(agent.topology(), None);
    }

    #[tokio::test]
    async fn default_transfer_governance_is_backward_compatible() {
        let agent = TestAgent { name: "test".to_string() };
        let request = AgentTransferRequest {
            invocation_id: "inv-1".to_string(),
            from: "test".to_string(),
            to: "peer".to_string(),
            depth: 1,
        };
        assert_eq!(agent.govern_transfer(&request).await.unwrap(), AgentTransferDecision::Allow);
    }
}
