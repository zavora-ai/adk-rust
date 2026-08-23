//! OpenAI supervisor-handoff team served through the runtime UI.

use std::sync::Arc;

use adk_agent::{LlmAgentBuilder, RelationshipKind, TeamMemberSpec, TeamRelationship, TeamSpec};
use adk_core::Agent;
use runtime_ui_showcase_example::{openai_model, serve};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = openai_model()?;
    let supervisor = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .description("Routes operational questions to one exact specialist")
            .instruction(
                "For billing questions, transfer to billing. For technical questions, transfer \
                 to technical. Always use transfer_to_agent and do not answer the specialist's part.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let billing = Arc::new(
        LlmAgentBuilder::new("billing")
            .description("Handles invoices, plans, payments, and credits")
            .instruction("Answer billing questions in concise Markdown. Do not transfer control.")
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let technical = Arc::new(
        LlmAgentBuilder::new("technical")
            .description("Handles errors, configuration, and service behavior")
            .instruction(
                "Answer technical questions in Markdown with a diagnosis and checklist. Do not transfer control.",
            )
            .model(model)
            .build()?,
    ) as Arc<dyn Agent>;
    let spec = TeamSpec {
        name: "support_team".to_string(),
        description: "Supervisor handoff with exact specialist allowlists".to_string(),
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
        policy: Default::default(),
    };
    let team = spec.compile([supervisor, billing, technical])?;
    serve(Arc::new(team) as Arc<dyn Agent>, "runtime-ui-team").await
}
