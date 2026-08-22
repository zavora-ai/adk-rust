//! Supervisor delegation to an agent-as-tool, followed by a supervisor answer.

use std::sync::Arc;

use adk_agent::{
    LlmAgentBuilder, RelationshipFailureStrategy, RelationshipKind, RelationshipPolicy,
    TeamHistoryPolicy, TeamLifecycleContext, TeamLifecycleDecision, TeamLifecycleHook,
    TeamLifecycleOutcome, TeamMemberSpec, TeamRelationship, TeamSpec, TeamStateMergePolicy,
};
use adk_core::Agent;
use async_trait::async_trait;
use team_architectures_example::{bounded_policy, openai_model, print_spec, run_team};

struct LifecyclePrinter;

#[async_trait]
impl TeamLifecycleHook for LifecyclePrinter {
    fn name(&self) -> &str {
        "example-lifecycle-printer"
    }

    async fn before(
        &self,
        context: &TeamLifecycleContext,
    ) -> adk_core::Result<TeamLifecycleDecision> {
        println!("[team lifecycle] starting {:?}", context.phase);
        Ok(TeamLifecycleDecision::Continue)
    }

    async fn after(
        &self,
        context: &TeamLifecycleContext,
        outcome: &TeamLifecycleOutcome,
    ) -> adk_core::Result<()> {
        println!("[team lifecycle] finished {:?}: {outcome:?}", context.phase);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = openai_model()?;
    let researcher = Arc::new(
        LlmAgentBuilder::new("researcher")
            .description("Produces a short evidence-oriented research note")
            .instruction(
                "Research the delegated question from your existing knowledge. Return three \
                 concise findings and clearly label uncertainty. Do not address the end user.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let supervisor = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .description("Delegates research, receives it, and answers the user")
            .instruction(
                "Always call the researcher tool before answering. Delegation is a call-and-return: \
                 use the returned research note, then give the user your own concise synthesis.",
            )
            .model(model)
            .build()?,
    ) as Arc<dyn Agent>;

    let spec = TeamSpec {
        name: "delegating_research_team".to_string(),
        description: "A supervisor calls one specialist and resumes with its result".to_string(),
        coordinator: "supervisor".to_string(),
        members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("researcher")],
        relationships: vec![TeamRelationship::new(
            "supervisor",
            "researcher",
            RelationshipKind::Delegate,
        )
        .with_policy(RelationshipPolicy {
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "request": { "type": "string" } },
                "required": ["request"],
                "additionalProperties": false
            })),
            history: TeamHistoryPolicy::Last { max_events: 8 },
            state_write_keys: vec!["temp:research_note".to_string()],
            state_merge: TeamStateMergePolicy::RejectConflicts,
            artifact_prefixes: vec!["research/".to_string()],
            timeout_ms: Some(30_000),
            failure: RelationshipFailureStrategy::Retry {
                max_attempts: 2,
                backoff_ms: 250,
            },
            ..RelationshipPolicy::default()
        })],
        policy: bounded_policy(),
    };
    print_spec(&spec)?;
    let team = spec.compile_with_hooks(
        [supervisor, researcher],
        [Arc::new(LifecyclePrinter) as Arc<dyn TeamLifecycleHook>],
    )?;
    println!(
        "Expected: supervisor calls researcher as a tool, receives its result, and remains in control."
    );
    run_team(
        team,
        "team-supervisor-delegation",
        "Summarize the main engineering tradeoffs of event sourcing for a small SaaS product.",
    )
    .await
}
