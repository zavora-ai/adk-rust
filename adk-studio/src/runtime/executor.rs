use crate::compiler::compile_agent;
use crate::schema::ProjectSchema;
use adk_core::Content;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use anyhow::{anyhow, Result};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

// Global session service persists across requests
fn session_service() -> &'static Arc<InMemorySessionService> {
    static INSTANCE: OnceLock<Arc<InMemorySessionService>> = OnceLock::new();
    INSTANCE.get_or_init(|| Arc::new(InMemorySessionService::new()))
}

/// Execute a project with a single message
pub async fn run_project(project: &ProjectSchema, input: &str, api_key: &str) -> Result<String> {
    let (agent_name, agent_schema) = project
        .agents
        .iter()
        .next()
        .ok_or_else(|| anyhow!("Project has no agents"))?;

    let agent = compile_agent(agent_name, agent_schema, api_key)?;
    let svc = session_service().clone();

    // Use project ID as session ID for persistence
    let session_id = project.id.to_string();

    // Get or create session
    let session = match svc.get(GetRequest {
        app_name: "studio".into(),
        user_id: "user".into(),
        session_id: session_id.clone(),
        num_recent_events: None,
        after: None,
    }).await {
        Ok(s) => s,
        Err(_) => svc.create(CreateRequest {
            app_name: "studio".into(),
            user_id: "user".into(),
            session_id: Some(session_id),
            state: HashMap::new(),
        }).await?
    };

    let runner = Runner::new(RunnerConfig {
        app_name: "studio".into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
    })?;

    let content = Content::new("user").with_text(input);
    let mut stream = runner.run("user".into(), session.id().to_string(), content).await?;

    let mut result = String::new();
    while let Some(Ok(event)) = stream.next().await {
        if let Some(c) = event.content() {
            for part in &c.parts {
                if let Some(text) = part.text() {
                    result.push_str(text);
                }
            }
        }
    }

    Ok(result)
}
