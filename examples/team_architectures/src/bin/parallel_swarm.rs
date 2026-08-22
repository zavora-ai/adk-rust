//! Parallel OpenAI research and review with an invocation-scoped shared state.

use std::sync::Arc;

use adk_agent::{LlmAgentBuilder, ParallelAgent, TeamMemberSpec, TeamSpec};
use adk_core::{Agent, Tool};
use team_architectures_example::{
    PublishSharedTool, ReadResearchTool, bounded_policy, openai_model, print_spec, run_team,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = openai_model()?;
    let facts = Arc::new(
        LlmAgentBuilder::new("facts_researcher")
            .description("Finds the strongest practical arguments and examples")
            .instruction(
                "Analyze the user's topic for practical facts and examples. Then you MUST call \
                 publish_facts once with your complete note in the content field.",
            )
            .model(model.clone())
            .tool(Arc::new(PublishSharedTool::new(
                "publish_facts",
                "Publish the completed facts note for the reviewer.",
                "facts",
            )) as Arc<dyn Tool>)
            .build()?,
    ) as Arc<dyn Agent>;
    let risks = Arc::new(
        LlmAgentBuilder::new("risk_researcher")
            .description("Finds counterarguments, failure modes, and uncertainty")
            .instruction(
                "Analyze the user's topic for risks, counterarguments, and uncertainty. Then you \
                 MUST call publish_risks once with your complete note in the content field.",
            )
            .model(model.clone())
            .tool(Arc::new(PublishSharedTool::new(
                "publish_risks",
                "Publish the completed risk note for the reviewer.",
                "risks",
            )) as Arc<dyn Tool>)
            .build()?,
    ) as Arc<dyn Agent>;
    let reviewer = Arc::new(
        LlmAgentBuilder::new("reviewer")
            .description("Waits for both research branches and reconciles them")
            .instruction(
                "First call read_research. It waits for both parallel research branches. Then \
                 produce a balanced recommendation that cites both the facts and risks notes.",
            )
            .model(model)
            .tool(Arc::new(ReadResearchTool) as Arc<dyn Tool>)
            .build()?,
    ) as Arc<dyn Agent>;

    let swarm = Arc::new(
        ParallelAgent::new("research_swarm", vec![facts, risks, reviewer])
            .with_shared_state()
            .with_description("Two researchers and a waiting reviewer run concurrently"),
    ) as Arc<dyn Agent>;
    let spec = TeamSpec {
        name: "parallel_research_team".to_string(),
        description: "A deterministic parallel composition with shared invocation state"
            .to_string(),
        coordinator: "research_swarm".to_string(),
        members: vec![TeamMemberSpec::new("research_swarm")],
        relationships: vec![],
        policy: bounded_policy(),
    };
    print_spec(&spec)?;
    let team = spec.compile([swarm])?;
    println!(
        "Expected: facts and risks run concurrently; reviewer blocks on shared state until both publish."
    );
    run_team(
        team,
        "team-parallel-swarm",
        "Should a ten-person engineering team adopt trunk-based development?",
    )
    .await
}
