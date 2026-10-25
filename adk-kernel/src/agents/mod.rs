//! Agents a person adds (spec 3.3 §5.7, §10 K7): by asking or by hand, kept by the kernel,
//! hosted beside the primary agent, and changed, paused or removed from the web home and the
//! terminal.
//!
//! | Item | Spec |
//! |---|---|
//! | [`AgentRecord`], [`AgentDefinition`], [`AgentRecordState`], [`AgentOrigin`], [`AvatarColor`], [`ModelChoice`], [`AgentUses`], [`FileAccess`], [`ShellAccess`], [`WebAccess`], [`agent_id_for`], [`check_agent_id`] | §5.7 "The definition", "Its identity" |
//! | [`AgentStore`], [`InMemoryAgentStore`](crate::stores::InMemoryAgentStore), `SqliteAgentStore`, `PostgresAgentStore`, `RedisAgentStore`, `MongoAgentStore`, `FirestoreAgentStore`, `Neo4jAgentStore` | §5.7 "Where it lives", K7 |
//! | [`AgentFactory`], [`AgentAssessment`], [`AgentCeiling`], [`AgentChoices`], [`HostedAgent`] | §5.7 "How an added agent runs", K7 |
//! | [`AgentCaller`], [`Presence`] | §5.7, §7.2, S-I18 |
//! | [`AGENT_CREATE_TOOL_NAME`] | §5.7 "The decision that adds an agent" |
//! | [`AgentAction`], [`AgentPreview`], [`EndedWork`], [`AgentChange`], [`AgentView`] | §5.7 "Changing an agent", "Pausing and removing" |
//! | [`kernel_assessment`], [`area_limits`], [`over_ceiling`], [`changed_fields`], [`widened`] | §5.7 "Risk", S-I12 |
//!
//! The kernel keeps every agent in its [`AgentStore`] and changes it only through its agent
//! table, which records each change before it acts (S-I17, S-I18). Agent definitions are
//! never read from workspace files, a watched directory or `adk-server`'s agent registry.

mod caller;
mod change;
mod check;
mod factory;
mod record;
mod store;

pub use caller::{AgentCaller, Presence};
pub use change::{AgentAction, AgentChange, AgentPreview, AgentView, EndedWork};
pub use check::{
    MONEY_PACKS, area_limits, changed_fields, kernel_assessment, over_ceiling, widened,
};
pub use factory::{AgentAssessment, AgentCeiling, AgentChoices, AgentFactory, HostedAgent};
pub use record::{
    AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentUses, AvatarColor,
    FALLBACK_AGENT_ID, FileAccess, MAX_AGENT_DESCRIPTION_LEN, MAX_AGENT_ID_LEN,
    MAX_AGENT_INSTRUCTIONS_LEN, MAX_AGENT_NAME_LEN, ModelChoice, RESERVED_AGENT_IDS,
    RESERVED_AGENT_NAME, ShellAccess, WebAccess, agent_id_for, check_agent_id,
};
pub use store::AgentStore;

/// The tool name of the inbox request that suggests an agent (spec 3.3 §5.7): a person
/// approves it as "Add this agent" or refuses it as "Don't add"; no standing rule, mode,
/// routine, chat or voice answers it yes (S-I11).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::agents::AGENT_CREATE_TOOL_NAME, "kernel:agent.create");
/// ```
pub const AGENT_CREATE_TOOL_NAME: &str = "kernel:agent.create";
