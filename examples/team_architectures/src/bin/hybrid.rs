//! Hybrid team: delegate to a deterministic workflow, then hand off control.

use std::sync::Arc;

use adk_agent::{
    LlmAgentBuilder, RelationshipKind, SequentialAgent, TeamMemberSpec, TeamRelationship, TeamSpec,
};
use adk_core::Agent;
use team_architectures_example::{bounded_policy, openai_model, print_spec, run_team};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = openai_model()?;
    let analyst = Arc::new(
        LlmAgentBuilder::new("workflow_analyst")
            .description("Extracts claims and constraints for a release note")
            .instruction(
                "Extract the important claims, constraints, and audience from the delegated request. \
                 Produce compact notes for the next workflow stage.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let drafter = Arc::new(
        LlmAgentBuilder::new("workflow_drafter")
            .description("Turns analysis into a release-note draft")
            .instruction(
                "Use the preceding analyst output to write a factual release-note draft under \
                 120 words. Return only the draft to the delegating supervisor.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let pipeline = Arc::new(
        SequentialAgent::new("draft_pipeline", vec![analyst, drafter])
            .with_description("Deterministic analysis-then-draft workflow"),
    ) as Arc<dyn Agent>;
    let publisher = Arc::new(
        LlmAgentBuilder::new("publisher")
            .description("Performs the final publication review")
            .instruction(
                "You have received control after a draft workflow completed. Read the draft from \
                 the conversation, remove unsupported claims, and return the final release note. \
                 Do not transfer control again.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let supervisor = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .description("Delegates drafting and hands approved work to publishing")
            .instruction(
                "For every request, first call draft_pipeline and inspect its returned draft. Then \
                 call transfer_to_agent with publisher. Delegation returns a result to you; the \
                 subsequent handoff transfers control permanently to the publisher.",
            )
            .model(model)
            .build()?,
    ) as Arc<dyn Agent>;

    let spec = TeamSpec {
        name: "hybrid_release_team".to_string(),
        description: "Delegation to a deterministic workflow followed by a handoff".to_string(),
        coordinator: "supervisor".to_string(),
        members: vec![
            TeamMemberSpec::new("supervisor"),
            TeamMemberSpec::new("draft_pipeline"),
            TeamMemberSpec::new("publisher"),
        ],
        relationships: vec![
            TeamRelationship::new("supervisor", "draft_pipeline", RelationshipKind::Delegate),
            TeamRelationship::new("supervisor", "publisher", RelationshipKind::Handoff),
        ],
        policy: bounded_policy(),
    };
    print_spec(&spec)?;
    let team = spec.compile([supervisor, pipeline, publisher])?;
    println!(
        "Expected: supervisor receives the sequential workflow result, then hands control to publisher."
    );
    run_team(
        team,
        "team-hybrid",
        "Prepare a release note: version 2.1 adds portable team specs, exact handoff allowlists, and bounded delegation depth.",
    )
    .await
}
