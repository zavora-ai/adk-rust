use anyhow::Result;
use adk_core::AgentLoader;
use adk_server::{ServerConfig, create_app};
use adk_session::InMemorySessionService;
use std::sync::Arc;

pub async fn run_serve(
    agent_loader: Arc<dyn AgentLoader>,
    port: u16,
) -> Result<()> {
    // Initialize telemetry
    if let Err(e) = adk_telemetry::init_telemetry("adk-server") {
        eprintln!("Failed to initialize telemetry: {}", e);
    }

    let session_service = Arc::new(InMemorySessionService::new());
    
    let config = ServerConfig {
        agent_loader,
        session_service,
        artifact_service: None,
    };
    
    let app = create_app(config);
    
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    
    println!("ADK Server starting on http://{}", addr);
    println!("Press Ctrl+C to stop");
    
    axum::serve(listener, app).await?;
    
    Ok(())
}
