use crate::compiler::compile_agent;
use crate::server::state::AppState;
use adk_core::Content;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn session_service() -> &'static Arc<InMemorySessionService> {
    static INSTANCE: OnceLock<Arc<InMemorySessionService>> = OnceLock::new();
    INSTANCE.get_or_init(|| Arc::new(InMemorySessionService::new()))
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "start")]
    Start { agent: String },
    #[serde(rename = "chunk")]
    Chunk { text: String },
    #[serde(rename = "end")]
    End,
    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Debug, Deserialize)]
struct RunRequest {
    input: String,
    api_key: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, id, state))
}

async fn handle_socket(mut socket: WebSocket, project_id: String, state: AppState) {
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(text) = msg {
            let req: RunRequest = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    let _ = send_event(&mut socket, StreamEvent::Error { message: e.to_string() })
                        .await;
                    continue;
                }
            };

            if let Err(e) = stream_response(&mut socket, &project_id, &req, &state).await {
                let _ =
                    send_event(&mut socket, StreamEvent::Error { message: e.to_string() }).await;
            }
        }
    }
}

async fn send_event(socket: &mut WebSocket, event: StreamEvent) -> Result<(), axum::Error> {
    let json = serde_json::to_string(&event).map_err(|e| axum::Error::new(e))?;
    socket.send(Message::Text(json.into())).await
}

async fn stream_response(
    socket: &mut WebSocket,
    project_id: &str,
    req: &RunRequest,
    state: &AppState,
) -> anyhow::Result<()> {
    let id: uuid::Uuid = project_id.parse()?;
    let storage = state.storage.read().await;
    let project = storage.get(id).await?;
    let (agent_name, agent_schema) =
        project.agents.iter().next().ok_or_else(|| anyhow::anyhow!("No agents"))?;

    let agent = compile_agent(agent_name, agent_schema, &req.api_key)?;
    let agent_name = agent_name.to_string();
    drop(storage);

    send_event(socket, StreamEvent::Start { agent: agent_name }).await?;

    let svc = session_service().clone();
    let session_id = project_id.to_string();

    let session = match svc
        .get(GetRequest {
            app_name: "studio".into(),
            user_id: "user".into(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
    {
        Ok(s) => s,
        Err(_) => {
            svc.create(CreateRequest {
                app_name: "studio".into(),
                user_id: "user".into(),
                session_id: Some(session_id),
                state: HashMap::new(),
            })
            .await?
        }
    };

    let runner = Runner::new(RunnerConfig {
        app_name: "studio".into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })?;

    let content = Content::new("user").with_text(&req.input);
    let mut stream = runner.run("user".into(), session.id().to_string(), content).await?;

    while let Some(Ok(event)) = stream.next().await {
        if let Some(c) = event.content() {
            for part in &c.parts {
                if let Some(text) = part.text() {
                    send_event(socket, StreamEvent::Chunk { text: text.to_string() }).await?;
                }
            }
        }
    }

    send_event(socket, StreamEvent::End).await?;
    Ok(())
}
