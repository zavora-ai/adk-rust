//! Governed graph nodes: a node whose call goes through the governed path, at most once
//! per lineage (spec 3.3 §12 developer track, §7.1–§7.4, §11 S-I1, S-I2).
//!
//! A graph opts a node in with [`CompiledGraph::with_governed_node`] or
//! [`GraphAgentBuilder::governed_node`](crate::agent::GraphAgentBuilder::governed_node).
//! Every dispatch of that node then runs this bracket instead of calling the node
//! directly:
//!
//! | Order | Check | Outcome |
//! |-------|-------|---------|
//! | 0 | Gate: the parent invocation, or the `RunConfig` of [`CompiledGraph::invoke_with_run_config`] | Without one the node is not run ([`GovernedError::NotRun`]) |
//! | 1 | Kill switch | Frozen: refused before anything is touched ([`GovernedError::Frozen`]) |
//! | 2 | Action-ledger lookup | A completed call is answered from its record and the effect log; a failed one fails again; one begun without an outcome fails closed |
//! | 3 | [`adk_core::authorize_tool_call`] | Pending: the node pauses and the call waits in the inbox. Refused: not run. Execute: begin, run, complete, append to the effect log |
//!
//! The ledger lookup comes before the governed path, unlike `LlmAgent`'s order: a call
//! that already happened is answered from its record, so it never creates a decision
//! (spec 3.3 §11 S-I1).
//!
//! The ledger key is `graph/{acting agent id}/{lineage root}/{node}/{occurrence}/{digest}`,
//! where the occurrence counts the node's completed runs in the lineage and the digest is
//! [`adk_core::json_digest`] of [`Node::call_args`]. A fork keeps the lineage root and the
//! occurrence counts, so a fork whose call is unchanged is answered from the original's
//! record, and a fork whose call changed waits for a person.
//!
//! A node resuming its own pause before a decision exists pauses again without asking
//! anyone again; a call that changed while it waited, or that a record answers, has its
//! waiting decision withdrawn. A governed node inside a subgraph keys its record by the
//! parent's lineage.
//!
//! The effect log thread `{lineage root}:effects` holds each executed call's writes. Only
//! this bracket writes it; forks never do, and checkpoint retention never prunes it, so
//! sibling forks find effects another fork ran.
//!
//! # Example
//!
//! ```
//! use adk_core::ToolEffect;
//! use adk_graph::edge::{END, START};
//! use adk_graph::governed::GovernedNode;
//! use adk_graph::graph::StateGraph;
//! use adk_graph::node::NodeOutput;
//!
//! let graph = StateGraph::with_channels(&["charge"])
//!     .add_node_fn("pay", |_ctx| async { Ok(NodeOutput::new()) })
//!     .add_edge(START, "pay")
//!     .add_edge("pay", END)
//!     .compile()?
//!     .with_node_effect("pay", ToolEffect::NonIdempotent)
//!     .with_governed_node("pay", GovernedNode::new().with_approval().spending_money());
//! assert_eq!(graph.shape().nodes[0].governed, Some(GovernedNode::new().with_approval().spending_money()));
//! # Ok::<(), adk_graph::GraphError>(())
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ActionOutcome, ActionRecord, ApprovalScope, GovernedCall, RunConfig, ToolAuthorization,
    ToolConfirmationRequest, ToolEffect, ToolGate, ToolOrigin,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::checkpoint::Checkpointer;
use crate::error::{GraphError, Result};
use crate::graph::CompiledGraph;
use crate::interrupt::GraphToolConfirmationPause;
use crate::node::{Node, NodeContext, NodeOutput};
use crate::record::NodeStatus;
use crate::state::{Checkpoint, State};

/// How a governed node is governed (spec 3.3 §12).
///
/// # Example
///
/// ```
/// use adk_graph::governed::GovernedNode;
///
/// let pay = GovernedNode::new().with_approval().spending_money();
/// assert!(pay.requires_approval && pay.spends_money);
/// assert_eq!(serde_json::to_value(pay)?, serde_json::json!({
///     "requiresApproval": true,
///     "spendsMoney": true,
/// }));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernedNode {
    /// Whether every call waits for a person, whatever the policy says.
    pub requires_approval: bool,
    /// Whether the call spends money, so surfaces treat its decision as high risk.
    pub spends_money: bool,
}

impl GovernedNode {
    /// A governed node that the policy alone decides and that spends no money.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call waits for a person.
    #[must_use]
    pub fn with_approval(mut self) -> Self {
        self.requires_approval = true;
        self
    }

    /// The call spends money.
    #[must_use]
    pub fn spending_money(mut self) -> Self {
        self.spends_money = true;
        self
    }
}

/// Why a governed node did not run, or failed without running (spec 3.3 §12).
///
/// A run reports these as [`GraphError::NodeExecutionFailed`] whose message starts with
/// [`code`](Self::code); [`GovernedError::code_of`] reads the code back.
///
/// # Example
///
/// ```
/// use adk_graph::GraphError;
/// use adk_graph::governed::GovernedError;
///
/// let error: GraphError = GovernedError::NotRun { node: "pay".to_string() }.into();
/// assert_eq!(GovernedError::code_of(&error), Some("graph.not_run"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GovernedError {
    /// The run has no approval gate, so the node is not run.
    #[error(
        "graph.not_run: governed node '{node}' was not run: the run has no approval gate. Run \
         the graph as a GraphAgent under a Runner, or with invoke_with_run_config"
    )]
    NotRun {
        /// The governed node.
        node: String,
    },
    /// The kill switch is frozen.
    #[error("graph.frozen: governed node '{node}' was not run: {reason}")]
    Frozen {
        /// The governed node.
        node: String,
        /// Why the kill switch refused it.
        reason: String,
    },
    /// The policy, a person, or the resumption check refused the call.
    #[error("graph.refused: governed node '{node}' was not run: {reason}")]
    Refused {
        /// The governed node.
        node: String,
        /// Why the call was refused.
        reason: String,
    },
    /// The call ran before and failed; it is not run again.
    #[error(
        "graph.recorded_failure: governed node '{node}' already ran this call and it failed: \
         {error}. It is not run again; fork with changed state to make a new call"
    )]
    RecordedFailure {
        /// The governed node.
        node: String,
        /// The error the call failed with.
        error: String,
    },
    /// The call began and recorded no outcome, so it may have taken effect.
    #[error(
        "graph.outcome_unknown: governed node '{node}' began this call before and recorded no \
         outcome, so it may have taken effect. It is not run again; check the effect, then \
         fork with changed state to make a new call (ledger key '{key}')"
    )]
    OutcomeUnknown {
        /// The governed node.
        node: String,
        /// The call's action-ledger key.
        key: String,
    },
    /// The call completed, but its writes are not in the effect log.
    #[error(
        "graph.effect_unavailable: governed node '{node}' already completed this call, but \
         its writes are not in the effect log. It is not run again (ledger key '{key}')"
    )]
    EffectUnavailable {
        /// The governed node.
        node: String,
        /// The call's action-ledger key.
        key: String,
    },
    /// An approver edited the call and the node cannot run an edited call.
    #[error(
        "graph.edit_unsupported: governed node '{node}' cannot run an edited call, so it was \
         not run. Approve the call as shown, or deny it"
    )]
    EditUnsupported {
        /// The governed node.
        node: String,
    },
    /// The action ledger, the approval gate or the effect log could not be read or written.
    #[error("graph.unavailable: governed node '{node}' was not run: {detail}")]
    Unavailable {
        /// The governed node.
        node: String,
        /// What could not be reached.
        detail: String,
    },
}

impl GovernedError {
    /// Every code a governed failure starts with.
    pub const CODES: [&'static str; 8] = [
        "graph.not_run",
        "graph.frozen",
        "graph.refused",
        "graph.recorded_failure",
        "graph.outcome_unknown",
        "graph.effect_unavailable",
        "graph.edit_unsupported",
        "graph.unavailable",
    ];

    /// The stable code this failure's message starts with.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::governed::GovernedError;
    ///
    /// let error = GovernedError::EditUnsupported { node: "pay".to_string() };
    /// assert_eq!(error.code(), "graph.edit_unsupported");
    /// assert!(error.to_string().starts_with("graph.edit_unsupported:"));
    /// ```
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotRun { .. } => Self::CODES[0],
            Self::Frozen { .. } => Self::CODES[1],
            Self::Refused { .. } => Self::CODES[2],
            Self::RecordedFailure { .. } => Self::CODES[3],
            Self::OutcomeUnknown { .. } => Self::CODES[4],
            Self::EffectUnavailable { .. } => Self::CODES[5],
            Self::EditUnsupported { .. } => Self::CODES[6],
            Self::Unavailable { .. } => Self::CODES[7],
        }
    }

    /// The governed code a graph error carries, if it is a governed failure.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::GraphError;
    /// use adk_graph::governed::GovernedError;
    ///
    /// let other = GraphError::NodeExecutionFailed { node: "x".into(), message: "boom".into() };
    /// assert_eq!(GovernedError::code_of(&other), None);
    /// ```
    pub fn code_of(error: &GraphError) -> Option<&'static str> {
        let GraphError::NodeExecutionFailed { message, .. } = error else { return None };
        Self::CODES
            .into_iter()
            .find(|code| message.strip_prefix(code).is_some_and(|rest| rest.starts_with(':')))
    }

    /// The node this failure belongs to.
    fn node(&self) -> &str {
        match self {
            Self::NotRun { node }
            | Self::Frozen { node, .. }
            | Self::Refused { node, .. }
            | Self::RecordedFailure { node, .. }
            | Self::OutcomeUnknown { node, .. }
            | Self::EffectUnavailable { node, .. }
            | Self::EditUnsupported { node }
            | Self::Unavailable { node, .. } => node,
        }
    }

    /// The status a node that failed this way is recorded with.
    fn status(&self) -> NodeStatus {
        match self {
            Self::NotRun { .. } => NodeStatus::NotRun,
            Self::Frozen { .. } | Self::Refused { .. } | Self::EditUnsupported { .. } => {
                NodeStatus::Refused
            }
            Self::RecordedFailure { .. }
            | Self::OutcomeUnknown { .. }
            | Self::EffectUnavailable { .. }
            | Self::Unavailable { .. } => NodeStatus::Failed,
        }
    }
}

impl From<GovernedError> for GraphError {
    fn from(error: GovernedError) -> Self {
        GraphError::NodeExecutionFailed {
            node: error.node().to_string(),
            message: error.to_string(),
        }
    }
}

/// Checkpoint metadata key of one effect-log entry.
pub const EFFECT_METADATA_KEY: &str = "adk.graph.effect";

/// What one executed governed call wrote, as the effect log keeps it (spec 3.3 §12).
///
/// # Example
///
/// ```
/// use adk_graph::governed::EffectRecord;
///
/// let effect = EffectRecord {
///     key: "graph/urn:adk:agent:app/invoice-run/t1/pay/1/fnv1a128:00".to_string(),
///     node: "pay".to_string(),
///     writes: [("charge".to_string(), serde_json::json!({ "id": "ch_1" }))].into(),
///     goto: None,
///     thread: "t1".to_string(),
///     checkpoint: "c2".to_string(),
///     at: chrono::Utc::now(),
/// };
/// assert_eq!(serde_json::to_value(&effect)?["writes"]["charge"]["id"], "ch_1");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectRecord {
    /// The call's action-ledger key.
    pub key: String,
    /// The governed node that made the call.
    pub node: String,
    /// The state updates the node returned.
    pub writes: BTreeMap<String, Value>,
    /// The successors the node named in place of its edges, if it named any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goto: Option<Vec<String>>,
    /// The thread the call ran on.
    pub thread: String,
    /// The checkpoint that records the step the call ran in.
    pub checkpoint: String,
    /// When the call completed, to the microsecond.
    pub at: DateTime<Utc>,
}

/// The effect-log thread of the lineage that started at thread `root`.
///
/// # Example
///
/// ```
/// assert_eq!(adk_graph::governed::effect_log_thread("t1"), "t1:effects");
/// ```
pub fn effect_log_thread(root: &str) -> String {
    format!("{root}:effects")
}

/// Every entry of the effect log of the lineage that started at thread `root`, oldest
/// first.
///
/// # Errors
///
/// Returns the checkpointer's error, or [`GraphError::SerializationError`] for a
/// malformed entry.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::MemoryCheckpointer;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_graph::Result<()> {
/// let checkpointer = MemoryCheckpointer::new();
/// assert!(adk_graph::governed::effect_log(&checkpointer, "t1").await?.is_empty());
/// # Ok(())
/// # }
/// ```
pub async fn effect_log(checkpointer: &dyn Checkpointer, root: &str) -> Result<Vec<EffectRecord>> {
    let entries = checkpointer.list(&effect_log_thread(root)).await?;
    entries
        .iter()
        .filter_map(|checkpoint| checkpoint.metadata.get(EFFECT_METADATA_KEY))
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| {
                GraphError::SerializationError(format!("effect log entry is malformed: {error}"))
            })
        })
        .collect()
}

/// Appends `entry` to the effect log of `root`, after its newest entry.
async fn append_effect(
    checkpointer: &dyn Checkpointer,
    root: &str,
    entry: &EffectRecord,
) -> Result<()> {
    let thread = effect_log_thread(root);
    let value = serde_json::to_value(entry)?;
    // Sibling forks append to one log; a conflict means another fork appended first.
    for _ in 0..16 {
        let (version, index) = checkpointer
            .load(&thread)
            .await?
            .map_or((1, 0), |newest| (newest.version + 1, newest.step + 1));
        let checkpoint = Checkpoint::new(&thread, State::new(), index, Vec::new())
            .with_version(version)
            .with_metadata(EFFECT_METADATA_KEY, value.clone());
        match checkpointer.save(&checkpoint).await {
            Ok(_) => return Ok(()),
            Err(GraphError::CheckpointConflict { .. }) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(GraphError::CheckpointError(format!(
        "the effect log '{thread}' kept changing while an entry was appended"
    )))
}

/// The agent name a governed node acts under when the run has no parent invocation.
pub(crate) const STANDALONE_AGENT_NAME: &str = "graph";

/// How many refused attempts one call may record before its key is exhausted.
const MAX_SLOTS: u32 = 16;

/// Everything one dispatch of a governed node needs, taken from the executor before the
/// step's futures start.
pub(crate) struct GovernedDispatch {
    pub(crate) node_name: String,
    pub(crate) node: Arc<dyn Node>,
    pub(crate) governed: GovernedNode,
    pub(crate) effect: ToolEffect,
    /// The node's shape entry, which its `ToolOrigin` binds to.
    pub(crate) declaration: Value,
    pub(crate) root: String,
    pub(crate) run_id: String,
    pub(crate) occurrence: u32,
    /// The request this node paused with, when this dispatch resumes that pause.
    pub(crate) held: Option<ToolConfirmationRequest>,
    pub(crate) checkpointer: Option<Arc<dyn Checkpointer>>,
    pub(crate) thread_id: String,
    /// The id of the checkpoint that will record this step.
    pub(crate) checkpoint_id: String,
    pub(crate) run_timeout: Option<Duration>,
}

/// What one dispatch of a governed node produced.
pub(crate) struct GovernedRun {
    pub(crate) output: Result<NodeOutput>,
    pub(crate) status: NodeStatus,
    pub(crate) effect_key: Option<String>,
    /// The request the node paused with, when it paused for a decision.
    pub(crate) held: Option<ToolConfirmationRequest>,
}

impl GovernedRun {
    fn failed(error: GovernedError, effect_key: Option<String>) -> Self {
        let status = error.status();
        Self { output: Err(error.into()), status, effect_key, held: None }
    }
}

impl CompiledGraph {
    /// Puts a node's calls on the governed path (spec 3.3 §12).
    ///
    /// The node then runs only through the approval gate of the run that holds it, at
    /// most once per call in its lineage; see the [module documentation](self). A graph
    /// that governs no node behaves as before.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::edge::{END, START};
    /// use adk_graph::governed::GovernedNode;
    /// use adk_graph::graph::StateGraph;
    /// use adk_graph::node::NodeOutput;
    ///
    /// let graph = StateGraph::with_channels(&["sent"])
    ///     .add_node_fn("notify", |_ctx| async { Ok(NodeOutput::new()) })
    ///     .add_edge(START, "notify")
    ///     .add_edge("notify", END)
    ///     .compile()?
    ///     .with_governed_node("notify", GovernedNode::new().with_approval());
    /// assert!(graph.shape().nodes[0].governed.is_some());
    /// # Ok::<(), adk_graph::GraphError>(())
    /// ```
    #[must_use]
    pub fn with_governed_node(mut self, node: &str, governed: GovernedNode) -> Self {
        self.governed_nodes.insert(node.to_string(), governed);
        self
    }

    /// Whether `node` is governed.
    pub(crate) fn is_governed(&self, node: &str) -> bool {
        self.governed_nodes.contains_key(node)
    }
}

/// Runs one dispatch of a governed node through the bracket described in the
/// [module documentation](self).
pub(crate) async fn dispatch(dispatch: GovernedDispatch, ctx: &NodeContext) -> GovernedRun {
    let node = dispatch.node_name.clone();
    let parent = ctx.config.parent_context.clone();
    let explicit = ctx.run_config();

    // 0. The gate. Without one the node never falls back to running ungoverned.
    let run_config: &RunConfig = match (&explicit, &parent) {
        (Some(config), _) => config,
        (None, Some(parent)) => parent.run_config(),
        (None, None) => {
            tracing::warn!(node = %node, "governed node not run: the run has no approval gate");
            return GovernedRun::failed(GovernedError::NotRun { node }, None);
        }
    };
    let gate = match (&explicit, &parent) {
        (None, Some(parent)) => ToolGate::for_context(parent.as_ref()),
        (_, Some(parent)) => ToolGate::new(
            run_config,
            parent.agent_name(),
            parent.invocation_id(),
            ApprovalScope::new(parent.app_name(), parent.user_id(), parent.session_id()),
        ),
        (_, None) => ToolGate::new(
            run_config,
            STANDALONE_AGENT_NAME,
            &dispatch.run_id,
            ApprovalScope::new(
                crate::node::STANDALONE_APP_NAME,
                crate::node::STANDALONE_USER_ID,
                &dispatch.thread_id,
            ),
        ),
    };
    let agent_name = parent.as_ref().map_or(STANDALONE_AGENT_NAME, |parent| parent.agent_name());

    // 1. The kill switch, before anything is touched.
    if let Err(frozen) = run_config.check_governance() {
        return GovernedRun::failed(GovernedError::Frozen { node, reason: frozen.message }, None);
    }

    let call_args = dispatch.node.call_args(ctx).unwrap_or_else(|| state_args(&ctx.state));
    let base_key = format!(
        "graph/{}/{}/{node}/{}/{}",
        gate.acting_context().agent.id,
        dispatch.root,
        dispatch.occurrence,
        adk_core::json_digest(&call_args)
    );

    // 2. The action ledger, before the governed path: a call that already happened is
    // answered from its record and creates no decision.
    let ledger = run_config.action_ledger.clone();
    let key = match &ledger {
        Some(ledger) => {
            let answered = match lookup(ledger.as_ref(), &node, &base_key).await {
                Lookup::Free(key) => Ok(key),
                Lookup::Done(key) => Err(replay(&dispatch, key).await),
                Lookup::Failed(run) => Err(*run),
            };
            match answered {
                Ok(key) => key,
                Err(run) => {
                    // The record answers the call, so a decision it waited for is moot. A
                    // ledger that could not be read answers nothing.
                    let unread = matches!(
                        &run.output,
                        Err(error) if GovernedError::code_of(error) == Some("graph.unavailable")
                    );
                    if let Some(held) = dispatch.held.as_ref().filter(|_| !unread) {
                        withdraw(&gate, held, &node).await;
                    }
                    return run;
                }
            }
        }
        None => base_key,
    };

    // A node resuming its own pause waits for that call's decision, as `LlmAgent` does: it
    // is not asked about again. A call whose arguments changed since is a new call, and
    // the hold for the old one is withdrawn.
    let mut resuming = None;
    if let Some(held) = dispatch.held.clone() {
        if held.args == call_args {
            match adk_core::confirmation_decision_available(&gate, &held).await {
                Ok(false) => {
                    tracing::debug!(node = %node, "governed node still waits for its decision");
                    let interrupt =
                        GraphToolConfirmationPause::pending_interrupt(node, held.clone());
                    return GovernedRun {
                        output: Ok(NodeOutput::new().with_interrupt(interrupt)),
                        status: NodeStatus::Paused,
                        effect_key: Some(key),
                        held: Some(held),
                    };
                }
                Ok(true) => resuming = Some(held),
                Err(error) => {
                    return GovernedRun::failed(
                        GovernedError::Unavailable {
                            node,
                            detail: format!(
                                "the approval store could not be read: {}",
                                error.redacted()
                            ),
                        },
                        Some(key),
                    );
                }
            }
        } else {
            withdraw(&gate, &held, &node).await;
        }
    }

    // 3. The governed path.
    let mut call = GovernedCall::new(
        node.clone(),
        format!("{}/{node}/{}", dispatch.run_id, dispatch.occurrence),
        call_args.clone(),
    )
    .with_effect(dispatch.effect)
    .requiring_confirmation(dispatch.governed.requires_approval);
    call.origin = Some(ToolOrigin::new(format!("graph:{agent_name}"), &dispatch.declaration, None));
    if let Some(held) = resuming {
        call = call.resuming(held);
    }
    let args = match adk_core::authorize_tool_call(&gate, call).await {
        Ok(ToolAuthorization::Execute { args, .. }) => args,
        Ok(ToolAuthorization::Pending { request, reason }) => {
            tracing::info!(node = %node, reason = %reason, "governed node waits for a decision");
            let interrupt = GraphToolConfirmationPause::pending_interrupt(node, request.clone());
            return GovernedRun {
                output: Ok(NodeOutput::new().with_interrupt(interrupt)),
                status: NodeStatus::Paused,
                effect_key: Some(key),
                held: Some(request),
            };
        }
        Ok(ToolAuthorization::Refuse { reason, .. }) => {
            tracing::info!(node = %node, "governed node refused");
            return GovernedRun::failed(GovernedError::Refused { node, reason }, Some(key));
        }
        Err(error) if error.code.starts_with("governance.") => {
            return GovernedRun::failed(
                GovernedError::Frozen { node, reason: error.message },
                None,
            );
        }
        Err(error) => {
            return GovernedRun::failed(
                GovernedError::Unavailable {
                    node,
                    detail: format!("the approval gate failed: {}", error.redacted()),
                },
                Some(key),
            );
        }
    };
    let edited = (args != call_args).then_some(args);

    // Begin, run, complete.
    let begun = match &ledger {
        Some(ledger) => {
            let record = ActionRecord::new(
                key.clone(),
                node.clone(),
                edited.as_ref().unwrap_or(&call_args),
                dispatch.effect,
            );
            match ledger.begin(&record).await {
                Ok(()) => Some(ledger.as_ref()),
                Err(error) if error.code == "tool.action_ledger.duplicate_key" => {
                    // Another run began this call between the lookup and now.
                    return GovernedRun::failed(
                        GovernedError::OutcomeUnknown { node, key: key.clone() },
                        Some(key),
                    );
                }
                Err(error) => {
                    return GovernedRun::failed(
                        GovernedError::Unavailable {
                            node,
                            detail: format!(
                                "the action ledger could not record the call: {}",
                                error.redacted()
                            ),
                        },
                        Some(key),
                    );
                }
            }
        }
        None => None,
    };
    // A freeze can land between the decision and the call.
    if let Err(frozen) = run_config.check_governance() {
        complete(
            begun,
            &node,
            &key,
            ActionOutcome::Failed { error: adk_core::not_run_text(&frozen) },
        )
        .await;
        return GovernedRun::failed(
            GovernedError::Frozen { node, reason: frozen.message },
            Some(key),
        );
    }

    crate::executor::begin_attempt(ctx);
    let ran = run_with_timeout(&node, dispatch.run_timeout, async {
        match edited {
            Some(args) => dispatch.node.run_call(ctx, args).await,
            None => dispatch.node.execute(ctx).await,
        }
    })
    .await;

    match ran {
        Ok(output) if output.interrupt.is_some() => {
            // The node paused instead of acting, so the call did not run.
            let note = adk_core::not_run_text(&adk_core::AdkError::agent(
                "the node paused before it acted",
            ));
            complete(begun, &node, &key, ActionOutcome::Failed { error: note }).await;
            GovernedRun {
                output: Ok(output),
                status: NodeStatus::Paused,
                effect_key: Some(key),
                held: None,
            }
        }
        Ok(output) => {
            let writes: BTreeMap<String, Value> =
                output.updates.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
            let digest = adk_core::json_digest(&serde_json::to_value(&writes).unwrap_or_default());
            complete(begun, &node, &key, ActionOutcome::Succeeded { result_digest: digest }).await;
            if let Some(checkpointer) = &dispatch.checkpointer {
                let entry = EffectRecord {
                    key: key.clone(),
                    node: node.clone(),
                    writes,
                    goto: output.goto.clone(),
                    thread: dispatch.thread_id.clone(),
                    checkpoint: dispatch.checkpoint_id.clone(),
                    at: crate::record::now(),
                };
                if let Err(error) =
                    append_effect(checkpointer.as_ref(), &dispatch.root, &entry).await
                {
                    // The call ran; a later replay fails closed with effect_unavailable.
                    tracing::error!(
                        node = %node,
                        error = %error,
                        "the effect log could not record a governed call's writes"
                    );
                }
            }
            GovernedRun {
                output: Ok(output),
                status: NodeStatus::Done,
                effect_key: Some(key),
                held: None,
            }
        }
        Err(error) if GovernedError::code_of(&error) == Some("graph.refused") => {
            // The node refused to send the call, such as for a changed credential.
            let note =
                adk_core::not_run_text(&adk_core::AdkError::agent("the node refused the call"));
            complete(begun, &node, &key, ActionOutcome::Failed { error: note }).await;
            GovernedRun {
                output: Err(error),
                status: NodeStatus::Refused,
                effect_key: Some(key),
                held: None,
            }
        }
        Err(error) if GovernedError::code_of(&error) == Some("graph.edit_unsupported") => {
            let note = adk_core::not_run_text(&adk_core::AdkError::agent(
                "the node cannot run an edited call",
            ));
            complete(begun, &node, &key, ActionOutcome::Failed { error: note }).await;
            GovernedRun::failed(GovernedError::EditUnsupported { node }, Some(key))
        }
        Err(error @ GraphError::NodeTimedOut { .. }) => {
            // The call may have taken effect, so its record stays begun: a replay fails
            // closed with outcome_unknown.
            tracing::warn!(node = %node, "governed node timed out; its outcome is unknown");
            GovernedRun {
                output: Err(error),
                status: NodeStatus::Failed,
                effect_key: Some(key),
                held: None,
            }
        }
        Err(error) => {
            complete(begun, &node, &key, ActionOutcome::Failed { error: error.to_string() }).await;
            GovernedRun {
                output: Err(error),
                status: NodeStatus::Failed,
                effect_key: Some(key),
                held: None,
            }
        }
    }
}

/// What the action ledger holds for a call.
enum Lookup {
    /// Nothing, or only refused attempts: the call may run under this key.
    Free(String),
    /// The call completed under this key.
    Done(String),
    /// The call cannot run: it failed, or its outcome is unknown, or the ledger failed.
    Failed(Box<GovernedRun>),
}

/// Reads the ledger for the call `base` names. A refused attempt is recorded as not run
/// and asks again under the next slot, `{base}#2`, because a ledger key begins once.
async fn lookup(ledger: &dyn adk_core::ActionLedger, node: &str, base: &str) -> Lookup {
    for slot in 1..=MAX_SLOTS {
        let key = if slot == 1 { base.to_string() } else { format!("{base}#{slot}") };
        let record = match ledger.get(&key).await {
            Ok(record) => record,
            Err(error) => {
                return Lookup::Failed(Box::new(GovernedRun::failed(
                    GovernedError::Unavailable {
                        node: node.to_string(),
                        detail: format!(
                            "the action ledger could not be read: {}",
                            error.redacted()
                        ),
                    },
                    Some(key),
                )));
            }
        };
        let Some(record) = record else { return Lookup::Free(key) };
        match record.outcome {
            Some(ActionOutcome::Succeeded { .. }) => return Lookup::Done(key),
            Some(ActionOutcome::Failed { error }) if adk_core::is_not_run(&error) => continue,
            Some(ActionOutcome::Failed { error }) => {
                return Lookup::Failed(Box::new(GovernedRun::failed(
                    GovernedError::RecordedFailure { node: node.to_string(), error },
                    Some(key),
                )));
            }
            // Begun without an outcome, or an outcome this version does not know.
            _ => {
                return Lookup::Failed(Box::new(GovernedRun::failed(
                    GovernedError::OutcomeUnknown { node: node.to_string(), key: key.clone() },
                    Some(key),
                )));
            }
        }
    }
    Lookup::Failed(Box::new(GovernedRun::failed(
        GovernedError::Unavailable {
            node: node.to_string(),
            detail: format!("the call was refused {MAX_SLOTS} times; fork to make a new call"),
        },
        Some(base.to_string()),
    )))
}

/// Answers a call that already completed from the effect log, without running it.
async fn replay(dispatch: &GovernedDispatch, key: String) -> GovernedRun {
    let node = dispatch.node_name.clone();
    let Some(checkpointer) = &dispatch.checkpointer else {
        return GovernedRun::failed(
            GovernedError::EffectUnavailable { node, key: key.clone() },
            Some(key),
        );
    };
    let effect = match effect_log(checkpointer.as_ref(), &dispatch.root).await {
        Ok(entries) => entries.into_iter().rev().find(|entry| entry.key == key),
        Err(error) => {
            return GovernedRun::failed(
                GovernedError::Unavailable {
                    node,
                    detail: format!("the effect log could not be read: {error}"),
                },
                Some(key),
            );
        }
    };
    let Some(effect) = effect else {
        return GovernedRun::failed(
            GovernedError::EffectUnavailable { node, key: key.clone() },
            Some(key),
        );
    };
    tracing::info!(node = %node, effect.key = %key, "governed call answered from its record");
    let mut output = NodeOutput::new().with_updates(effect.writes.into_iter().collect());
    output.goto = effect.goto;
    GovernedRun {
        output: Ok(output),
        status: NodeStatus::Recorded,
        effect_key: Some(key),
        held: None,
    }
}

/// Withdraws the decision a governed node waited for when it will not wait any more.
async fn withdraw(gate: &ToolGate<'_>, held: &ToolConfirmationRequest, node: &str) {
    if let Err(error) = adk_core::withdraw_held_call(gate, held).await {
        tracing::warn!(node = %node, error = %error, "a governed node's waiting decision could not be withdrawn");
    }
}

/// Records a begun call's outcome; a failure is logged, and a replay then reports the
/// outcome as unknown.
async fn complete(
    ledger: Option<&dyn adk_core::ActionLedger>,
    node: &str,
    key: &str,
    outcome: ActionOutcome,
) {
    let Some(ledger) = ledger else { return };
    if let Err(error) = ledger.complete(key, outcome).await {
        tracing::error!(
            node = %node,
            effect.key = %key,
            error = %error,
            "the action ledger could not record a governed call's outcome"
        );
    }
}

/// Runs a governed node's call under its run timeout, once: a timed-out call is never
/// retried, because it may have taken effect.
async fn run_with_timeout(
    node: &str,
    timeout: Option<Duration>,
    call: impl std::future::Future<Output = Result<NodeOutput>>,
) -> Result<NodeOutput> {
    match timeout {
        Some(limit) => tokio::time::timeout(limit, call).await.unwrap_or_else(|_| {
            Err(GraphError::NodeTimedOut { node: node.to_string(), elapsed: limit })
        }),
        None => call.await,
    }
}

/// The call of a node that does not describe one: the whole state it reads.
fn state_args(state: &State) -> Value {
    let sorted: BTreeMap<&String, &Value> = state.iter().collect();
    serde_json::to_value(sorted).unwrap_or(Value::Null)
}
