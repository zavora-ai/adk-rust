//! `CompiledTeam::close` reaches the coordinator's and every member's toolsets, so
//! `Runner::close` on a team root releases what its agents hold — MCP connections,
//! container sessions — as it does on a plain `LlmAgent` root.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::{
    LlmAgentBuilder, RelationshipKind, TeamMemberSpec, TeamPolicy, TeamRelationship, TeamSpec,
};
use adk_core::{Agent, ReadonlyContext, Result, Tool, Toolset};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use async_trait::async_trait;

/// A toolset that counts how often it is closed.
struct CountingToolset {
    name: String,
    closed: Arc<AtomicUsize>,
}

#[async_trait]
impl Toolset for CountingToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(Vec::new())
    }

    async fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

fn agent(name: &str, closed: &Arc<AtomicUsize>) -> Arc<dyn Agent> {
    let model = Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let toolset = CountingToolset { name: format!("{name}-tools"), closed: closed.clone() };
    Arc::new(LlmAgentBuilder::new(name).model(model).toolset(Arc::new(toolset)).build().unwrap())
}

fn team(lead: Arc<dyn Agent>, helper: Arc<dyn Agent>) -> Arc<adk_agent::CompiledTeam> {
    Arc::new(
        TeamSpec {
            name: "team".to_string(),
            description: String::new(),
            coordinator: "lead".to_string(),
            members: vec![TeamMemberSpec::new("lead"), TeamMemberSpec::new("helper")],
            relationships: vec![TeamRelationship::new("lead", "helper", RelationshipKind::Handoff)],
            policy: TeamPolicy::default(),
        }
        .compile([lead, helper])
        .unwrap(),
    )
}

#[tokio::test]
async fn closing_a_compiled_team_closes_the_coordinator_and_every_member_once() {
    let lead_closed = Arc::new(AtomicUsize::new(0));
    let helper_closed = Arc::new(AtomicUsize::new(0));
    let team = team(agent("lead", &lead_closed), agent("helper", &helper_closed));

    team.close().await;

    assert_eq!(
        (lead_closed.load(Ordering::SeqCst), helper_closed.load(Ordering::SeqCst)),
        (1, 1),
        "the coordinator's and the member's toolsets close once each"
    );
}

#[tokio::test]
async fn runner_close_reaches_the_coordinator_toolsets_through_the_team_root() {
    let lead_closed = Arc::new(AtomicUsize::new(0));
    let helper_closed = Arc::new(AtomicUsize::new(0));
    let team = team(agent("lead", &lead_closed), agent("helper", &helper_closed));
    let runner = Runner::builder()
        .app_name("team-app")
        .agent(team)
        .session_service(Arc::new(InMemorySessionService::new()))
        .build()
        .unwrap();

    runner.close().await;

    assert_eq!((lead_closed.load(Ordering::SeqCst), helper_closed.load(Ordering::SeqCst)), (1, 1));
}
