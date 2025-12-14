use crate::schema::{AgentSchema, AgentType};
use adk_agent::{Agent, LlmAgentBuilder};
use adk_model::gemini::GeminiModel;
use anyhow::{anyhow, Result};
use std::sync::Arc;

/// Compile an AgentSchema into a runnable Agent
pub fn compile_agent(name: &str, schema: &AgentSchema, api_key: &str) -> Result<Arc<dyn Agent>> {
    match schema.agent_type {
        AgentType::Llm => compile_llm_agent(name, schema, api_key),
        _ => Err(anyhow!("Agent type {:?} not yet supported", schema.agent_type)),
    }
}

fn compile_llm_agent(name: &str, schema: &AgentSchema, api_key: &str) -> Result<Arc<dyn Agent>> {
    let model_name = schema.model.as_deref().unwrap_or("gemini-2.0-flash");
    let model = Arc::new(GeminiModel::new(api_key, model_name)?);

    let mut builder = LlmAgentBuilder::new(name).model(model);

    if !schema.instruction.is_empty() {
        builder = builder.instruction(&schema.instruction);
    }

    Ok(Arc::new(builder.build()?))
}
