//! Supervisor handoff to one of two exact specialist targets.

use std::sync::Arc;

use adk_agent::{LlmAgentBuilder, RelationshipKind, TeamMemberSpec, TeamRelationship, TeamSpec};
use adk_core::Agent;
use team_architectures_example::{bounded_policy, openai_model, print_spec, run_team};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = openai_model()?;
    let billing = Arc::new(
        LlmAgentBuilder::new("billing")
            .description("Resolves invoices, payments, and subscription charges")
            .instruction(
                "You are the billing specialist. Answer the user's billing question directly. \
                 You are now in control; do not try to return to the supervisor.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let technical = Arc::new(
        LlmAgentBuilder::new("technical")
            .description("Troubleshoots errors, configuration, and product behavior")
            .instruction(
                "You are the technical specialist. Give concise troubleshooting steps. \
                 You are now in control; do not try to return to the supervisor.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let supervisor = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .description("Routes each request to the one specialist allowed to handle it")
            .instruction(
                "You supervise billing and technical support. For any billing request, call \
                 transfer_to_agent with billing. For any technical request, call it with \
                 technical. A handoff transfers control: do not answer the specialist's part.",
            )
            .model(model)
            .build()?,
    ) as Arc<dyn Agent>;

    let spec = TeamSpec {
        name: "support_team".to_string(),
        description: "A supervisor hands control to exactly one allowed specialist".to_string(),
        coordinator: "supervisor".to_string(),
        members: vec![
            TeamMemberSpec::new("supervisor"),
            TeamMemberSpec::new("billing"),
            TeamMemberSpec::new("technical"),
        ],
        relationships: vec![
            TeamRelationship::new("supervisor", "billing", RelationshipKind::Handoff),
            TeamRelationship::new("supervisor", "technical", RelationshipKind::Handoff),
        ],
        policy: bounded_policy(),
    };
    print_spec(&spec)?;
    let team = spec.compile([supervisor, billing, technical])?;
    println!(
        "Expected: supervisor hands off to billing; billing cannot hand off to technical or back."
    );
    run_team(
        team,
        "team-supervisor-handoff",
        "My latest invoice contains the same subscription charge twice. What should I do?",
    )
    .await
}
