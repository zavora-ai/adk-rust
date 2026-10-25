//! The agents a person adds, read over the control plane (spec 3.3 §5.7, §10 K7).
//!
//! | Route (under `/api`) | Caller | Does |
//! |---|---|---|
//! | `GET /kernel/agents` | operator, or a device with `read` | Every agent with its state — suggested, active, paused, being removed, removed — but refused suggestions, with why this kernel cannot start one ([`AgentsResponse`]) |
//! | `GET /kernel/agents/{id}` | operator, or a device with `read` | One agent, tombstones included ([`AgentView`]); `404` for an unknown id |
//!
//! No route creates, changes, pauses or removes an agent: changes go through the kernel's
//! agent table from the web home's engine in the kernel's process, and from the terminal
//! through the `/agent` command, which `adk-rust up` serves with the REPL's other commands.
//! The read routes sit under `/api/kernel/agents` because `adk-server`'s agent registry
//! already serves `/api/agents`.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::routing::get;
use serde::{Deserialize, Serialize};

use super::paths;
use super::routes::{Caller, ControlBackend};
use crate::agents::AgentView;
use crate::error::{AGENT_NOT_FOUND_CODE, KernelError, Result};

/// `GET /kernel/agents` answer (spec 3.3 §10 K7).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::AgentsResponse;
///
/// let answer: AgentsResponse = serde_json::from_value(serde_json::json!({ "agents": [] })).unwrap();
/// assert!(answer.agents.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsResponse {
    /// The agents with their states, removed ones included, ordered by id.
    pub agents: Vec<AgentView>,
}

type Backend = State<Arc<dyn ControlBackend>>;

pub(super) fn routes() -> Router<Arc<dyn ControlBackend>> {
    Router::new().route(paths::AGENTS, get(list)).route(paths::AGENT, get(one))
}

fn table(backend: &dyn ControlBackend) -> Result<&crate::agents::Agents> {
    backend.kernel().map(crate::Kernel::agents).ok_or_else(|| KernelError::Config {
        reason: "this control plane serves no kernel, so it keeps no agents".into(),
    })
}

async fn list(State(backend): Backend, caller: Caller) -> Result<Json<AgentsResponse>> {
    caller.require_operator("read the agents")?;
    let agents = table(backend.as_ref())?.list().await?;
    Ok(Json(AgentsResponse { agents }))
}

async fn one(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<AgentView>> {
    caller.require_operator("read an agent")?;
    match table(backend.as_ref())?.get(&id).await? {
        Some(view) => Ok(Json(view)),
        None => Err(KernelError::Core(adk_core::AdkError::new(
            adk_core::ErrorComponent::Server,
            adk_core::ErrorCategory::NotFound,
            AGENT_NOT_FOUND_CODE,
            format!("no agent `{id}`; list them with `adk-rust agents`"),
        ))),
    }
}
