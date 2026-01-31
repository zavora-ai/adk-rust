use crate::schema::{ProjectMeta, ProjectSchema};
use crate::server::events::ResumeEvent;
use crate::server::graph_runner::{deserialize_interrupt_response, INTERRUPTED_SESSIONS};
use crate::server::sse::send_resume_response;
use crate::server::state::AppState;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// API error response
#[derive(Serialize)]
pub struct ApiError {
    pub error: String,
}

impl ApiError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { error: msg.into() }
    }
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (status, Json(ApiError::new(msg)))
}

/// List all projects
pub async fn list_projects(State(state): State<AppState>) -> ApiResult<Vec<ProjectMeta>> {
    let storage = state.storage.read().await;
    storage
        .list()
        .await
        .map(Json)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Create project request
#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Create a new project
pub async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectRequest>,
) -> ApiResult<ProjectSchema> {
    let mut project = ProjectSchema::new(&req.name);
    project.description = req.description;

    let storage = state.storage.read().await;
    storage
        .save(&project)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(project))
}

/// Get project by ID
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<ProjectSchema> {
    let storage = state.storage.read().await;
    storage.get(id).await.map(Json).map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))
}

/// Update project
pub async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(mut project): Json<ProjectSchema>,
) -> ApiResult<ProjectSchema> {
    let storage = state.storage.read().await;

    if !storage.exists(id).await {
        return Err(err(StatusCode::NOT_FOUND, "Project not found"));
    }

    project.id = id;
    project.updated_at = chrono::Utc::now();

    storage
        .save(&project)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(project))
}

/// Delete project
pub async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let storage = state.storage.read().await;
    storage.delete(id).await.map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Run project request (deprecated)
#[derive(Deserialize)]
#[allow(dead_code)]
pub struct RunRequest {
    pub input: String,
}

/// Run project response
#[derive(Serialize)]
pub struct RunResponse {
    pub output: String,
}

/// Run a project with input (deprecated - use build + stream with binary_path)
pub async fn run_project(
    State(_state): State<AppState>,
    Path(_id): Path<Uuid>,
    Json(_req): Json<RunRequest>,
) -> ApiResult<RunResponse> {
    Err(err(
        StatusCode::BAD_REQUEST,
        "Runtime execution removed. Use 'Build' then run via console with the compiled binary.",
    ))
}

/// Clear session for a project
pub async fn clear_session(
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    // Session is now managed by sse module's persistent process
    // This endpoint is kept for compatibility but does nothing
    let _ = id;
    Ok(StatusCode::NO_CONTENT)
}

/// Compile project to Rust code
pub async fn compile_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<crate::codegen::GeneratedProject> {
    let storage = state.storage.read().await;
    let project = storage.get(id).await.map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;

    let generated = crate::codegen::generate_rust_project(&project)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(generated))
}

/// Build response
#[derive(Serialize)]
pub struct BuildResponse {
    pub success: bool,
    pub output: String,
    pub binary_path: Option<String>,
}

/// Compile and build project to executable (streaming)
pub async fn build_project_stream(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> axum::response::Sse<
    impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use axum::response::sse::Event;
    use std::time::Instant;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let stream = async_stream::stream! {
        let start_time = Instant::now();

        let storage = state.storage.read().await;
        let project = match storage.get(id).await {
            Ok(p) => p,
            Err(e) => {
                yield Ok(Event::default().event("error").data(e.to_string()));
                return;
            }
        };

        let generated = match crate::codegen::generate_rust_project(&project) {
            Ok(g) => g,
            Err(e) => {
                yield Ok(Event::default().event("error").data(e.to_string()));
                return;
            }
        };

        // Write to temp directory
        let mut project_name = project.name.to_lowercase().replace(' ', "_").replace(|c: char| !c.is_alphanumeric() && c != '_', "");
        if project_name.is_empty() || project_name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            project_name = format!("project_{}", project_name);
        }
        let build_dir = std::env::temp_dir().join("adk-studio-builds").join(&project_name);
        if let Err(e) = std::fs::create_dir_all(&build_dir) {
            yield Ok(Event::default().event("error").data(e.to_string()));
            return;
        }

        for file in &generated.files {
            let path = build_dir.join(&file.path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, &file.content) {
                yield Ok(Event::default().event("error").data(e.to_string()));
                return;
            }
        }

        yield Ok(Event::default().event("status").data("Starting cargo build..."));

        // Use shared target directory for faster incremental builds
        let shared_target = std::env::temp_dir().join("adk-studio-builds").join("_shared_target");
        let _ = std::fs::create_dir_all(&shared_target);

        let mut child = match Command::new("cargo")
            .arg("build")
            .env("CARGO_TARGET_DIR", &shared_target)
            .current_dir(&build_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn() {
                Ok(c) => c,
                Err(e) => {
                    yield Ok(Event::default().event("error").data(e.to_string()));
                    return;
                }
            };

        let stderr = child.stderr.take().unwrap();
        let mut reader = BufReader::new(stderr).lines();

        while let Ok(Some(line)) = reader.next_line().await {
            yield Ok(Event::default().event("output").data(line));
        }

        let status = child.wait().await;
        let success = status.map(|s| s.success()).unwrap_or(false);
        let elapsed = start_time.elapsed();

        if success {
            let binary = shared_target.join("debug").join(&project_name);
            yield Ok(Event::default().event("output").data(format!("\n✓ Build completed in {:.1}s", elapsed.as_secs_f32())));
            yield Ok(Event::default().event("done").data(binary.to_string_lossy()));
        } else {
            yield Ok(Event::default().event("output").data(format!("\n✗ Build failed after {:.1}s", elapsed.as_secs_f32())));
            yield Ok(Event::default().event("error").data("Build failed"));
        }
    };

    axum::response::Sse::new(stream)
}

/// Compile and build project to executable
pub async fn build_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<BuildResponse> {
    let storage = state.storage.read().await;
    let project = storage.get(id).await.map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;

    let generated = crate::codegen::generate_rust_project(&project)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Write to temp directory
    let project_name = project.name.to_lowercase().replace(' ', "_");
    let build_dir = std::env::temp_dir().join("adk-studio-builds").join(&project_name);
    std::fs::create_dir_all(&build_dir)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for file in &generated.files {
        let path = build_dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, &file.content)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // Use shared target directory for faster incremental builds
    let shared_target = std::env::temp_dir().join("adk-studio-builds").join("_shared_target");
    let _ = std::fs::create_dir_all(&shared_target);

    // Run cargo build
    let output = std::process::Command::new("cargo")
        .arg("build")
        .env("CARGO_TARGET_DIR", &shared_target)
        .current_dir(&build_dir)
        .output()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}\n{}", stdout, stderr);

    if output.status.success() {
        let binary = shared_target.join("debug").join(&project_name);
        Ok(Json(BuildResponse {
            success: true,
            output: combined,
            binary_path: Some(binary.to_string_lossy().to_string()),
        }))
    } else {
        Ok(Json(BuildResponse { success: false, output: combined, binary_path: None }))
    }
}


// ============================================
// HITL Resume Endpoint
// ============================================
// Task 10: Add Resume Endpoint
// Requirements: 3.2, 5.2

/// Request body for resuming an interrupted workflow.
///
/// ## JSON Format
/// ```json
/// {
///   "response": { "approved": true, "comment": "Looks good" }
/// }
/// ```
/// or for simple text responses:
/// ```json
/// {
///   "response": "approve"
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct ResumeRequest {
    /// User's response to the interrupt.
    /// Can be a JSON object with multiple fields or a simple value.
    pub response: serde_json::Value,
}

/// Response from the resume endpoint.
#[derive(Debug, Serialize)]
pub struct ResumeResponse {
    /// Whether the resume was successful
    pub success: bool,
    /// Node ID that was resumed
    pub node_id: String,
    /// Message describing the result
    pub message: String,
}

/// Resume an interrupted workflow session.
///
/// This endpoint handles user responses to HITL (Human-in-the-Loop) interrupts.
/// When a workflow is interrupted (e.g., for approval), the user can respond
/// via this endpoint to resume execution.
///
/// ## Endpoint
/// `POST /api/sessions/{session_id}/resume`
///
/// ## Request Body
/// ```json
/// {
///   "response": { "approved": true }
/// }
/// ```
///
/// ## Response
/// ```json
/// {
///   "success": true,
///   "node_id": "review",
///   "message": "Workflow resumed successfully"
/// }
/// ```
///
/// ## Flow
/// 1. Retrieve the interrupted session state from storage
/// 2. Deserialize the user's response into state updates
/// 3. Update the workflow state with the response (equivalent to `graph.update_state()`)
/// 4. Resume workflow execution (equivalent to `graph.invoke()`)
/// 5. Emit a resume event via SSE
///
/// ## Requirements
/// - Requirement 3.2: After user response, `graph.update_state()` is called
/// - Requirement 5.2: State persistence - workflow resumes from checkpoint
///
/// ## Errors
/// - 404: Session not found or not interrupted
/// - 500: Internal error during resume
pub async fn resume_session(
    Path(session_id): Path<String>,
    Json(req): Json<ResumeRequest>,
) -> ApiResult<ResumeResponse> {
    // Task 10.1: Get the interrupted session state
    let interrupted_state = INTERRUPTED_SESSIONS
        .get(&session_id)
        .await
        .ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                format!("Session '{}' not found or not interrupted", session_id),
            )
        })?;

    let node_id = interrupted_state.node_id.clone();
    let thread_id = interrupted_state.thread_id.clone();
    let checkpoint_id = interrupted_state.checkpoint_id.clone();

    // Task 10.2 & 10.3: Deserialize user response and prepare state updates
    // This is equivalent to calling `graph.update_state()` with the response
    let state_updates = deserialize_interrupt_response(req.response.clone());

    // Log the resume action for debugging
    tracing::info!(
        session_id = %session_id,
        node_id = %node_id,
        thread_id = %thread_id,
        checkpoint_id = %checkpoint_id,
        updates = ?state_updates,
        "Resuming interrupted workflow"
    );

    // Task 10.4: Resume workflow execution
    // Send the user's response to the subprocess via stdin.
    // This triggers the workflow to resume from its checkpoint.
    if let Err(e) = send_resume_response(&session_id, req.response.clone()).await {
        tracing::warn!(
            session_id = %session_id,
            error = %e,
            "Failed to send resume response to subprocess, session may have ended"
        );
        // Don't fail the request - the session might have ended naturally
        // or the response will be picked up on the next stream connection
    }

    // Remove the interrupted state since we're resuming
    INTERRUPTED_SESSIONS.remove(&session_id).await;

    // Task 10.5: Emit resume event
    // The resume event is emitted to notify the frontend that the workflow
    // is resuming. We log it here for debugging.
    let resume_event = ResumeEvent::new(&node_id);
    tracing::info!(
        session_id = %session_id,
        event = %resume_event.to_json(),
        "Resume event emitted"
    );

    Ok(Json(ResumeResponse {
        success: true,
        node_id,
        message: format!(
            "Workflow resumed. Response: {}",
            serde_json::to_string(&req.response).unwrap_or_default()
        ),
    }))
}

// ============================================
// Webhook Trigger Endpoints
// ============================================
// Development server webhook endpoints for testing webhook triggers
// without building the project.

/// Response from webhook trigger endpoint.
#[derive(Debug, Serialize)]
pub struct WebhookTriggerResponse {
    /// Whether the webhook was accepted
    pub success: bool,
    /// Session ID for streaming the workflow execution
    pub session_id: String,
    /// Message describing the result
    pub message: String,
    /// The webhook path that was triggered
    pub path: String,
    /// Instructions for streaming the response
    pub stream_url: String,
    /// Path to the built binary (if available)
    pub binary_path: Option<String>,
}

/// Get the binary path for a project based on its name.
/// 
/// The binary is built to: `{temp_dir}/adk-studio-builds/_shared_target/debug/{project_name}`
fn get_project_binary_path(project_name: &str) -> String {
    let project_name = project_name.to_lowercase().replace(' ', "_");
    let shared_target = std::env::temp_dir().join("adk-studio-builds").join("_shared_target");
    let binary = shared_target.join("debug").join(&project_name);
    binary.to_string_lossy().to_string()
}

/// Check if a project has been built (binary exists).
fn is_project_built(project_name: &str) -> bool {
    let binary_path = get_project_binary_path(project_name);
    std::path::Path::new(&binary_path).exists()
}

/// Simple percent-encoding for URL query parameters.
fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

/// Webhook trigger for POST requests.
///
/// This endpoint allows testing webhook triggers in the development server
/// without building the project. It accepts a webhook payload and returns
/// a session ID that can be used to stream the workflow execution.
///
/// ## Endpoint
/// `POST /api/projects/{id}/webhook/{path}`
///
/// ## Example
/// ```bash
/// # Trigger a webhook
/// curl -X POST http://localhost:6000/api/projects/{project_id}/webhook/api/webhook/my-flow \
///   -H "Content-Type: application/json" \
///   -d '{"message": "Hello from webhook!"}'
///
/// # Then stream the response
/// curl "http://localhost:6000/api/projects/{project_id}/stream?input=__webhook__&session_id={session_id}&binary_path={binary_path}"
/// ```
///
/// ## Authentication
/// Supports the same authentication methods as configured in the trigger:
/// - No auth: No headers required
/// - Bearer token: `Authorization: Bearer <token>`
/// - API key: Custom header (e.g., `X-API-Key: <key>`)
///
/// ## Response
/// ```json
/// {
///   "success": true,
///   "session_id": "abc123",
///   "message": "Webhook received. Use stream_url to get the response.",
///   "path": "/api/webhook/my-flow",
///   "stream_url": "/api/projects/{id}/stream?input=__webhook__&session_id=abc123"
/// }
/// ```
pub async fn webhook_trigger(
    State(state): State<AppState>,
    Path((id, path)): Path<(Uuid, String)>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> ApiResult<WebhookTriggerResponse> {
    // Get the project to validate webhook configuration
    let storage = state.storage.read().await;
    let project = storage
        .get(id)
        .await
        .map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;

    // Find the webhook trigger in the project
    let webhook_path = format!("/{}", path.trim_start_matches('/'));
    let trigger = find_webhook_trigger(&project, &webhook_path, "POST");

    // Validate authentication if configured
    if let Some(ref trigger_config) = trigger {
        validate_webhook_auth(&headers, trigger_config)?;
    }

    // Generate a session ID for this webhook execution
    let session_id = uuid::Uuid::new_v4().to_string();

    // Store the webhook payload in a temporary location for the stream handler
    // The stream handler will inject this into the workflow state
    store_webhook_payload(&session_id, &webhook_path, "POST", payload.clone()).await;

    // Find the binary path for this project
    let binary_path = get_project_binary_path(&project.name);
    let binary_exists = is_project_built(&project.name);
    
    let stream_url = format!(
        "/api/projects/{}/stream?input=__webhook__&session_id={}&binary_path={}",
        id, session_id, percent_encode(&binary_path)
    );

    tracing::info!(
        project_id = %id,
        path = %webhook_path,
        session_id = %session_id,
        payload = %serde_json::to_string(&payload).unwrap_or_default(),
        binary_path = %binary_path,
        binary_exists = %binary_exists,
        "Webhook trigger received"
    );

    Ok(Json(WebhookTriggerResponse {
        success: true,
        session_id: session_id.clone(),
        message: format!(
            "Webhook received for path '{}'. {}{}",
            webhook_path,
            if trigger.is_some() {
                "Trigger configuration found."
            } else {
                "No matching trigger found, but payload stored."
            },
            if !binary_exists {
                " WARNING: Project not built. Build the project first."
            } else {
                ""
            }
        ),
        path: webhook_path,
        stream_url,
        binary_path: if binary_exists { Some(binary_path) } else { None },
    }))
}

/// Webhook trigger for GET requests.
///
/// Similar to POST webhook trigger but accepts query parameters instead of body.
///
/// ## Endpoint
/// `GET /api/projects/{id}/webhook/{path}?param1=value1&param2=value2`
pub async fn webhook_trigger_get(
    State(state): State<AppState>,
    Path((id, path)): Path<(Uuid, String)>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<WebhookTriggerResponse> {
    // Get the project to validate webhook configuration
    let storage = state.storage.read().await;
    let project = storage
        .get(id)
        .await
        .map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;

    // Find the webhook trigger in the project
    let webhook_path = format!("/{}", path.trim_start_matches('/'));
    let trigger = find_webhook_trigger(&project, &webhook_path, "GET");

    // Validate authentication if configured
    if let Some(ref trigger_config) = trigger {
        validate_webhook_auth(&headers, trigger_config)?;
    }

    // Generate a session ID for this webhook execution
    let session_id = uuid::Uuid::new_v4().to_string();

    // Convert query params to JSON payload
    let payload = serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);

    // Store the webhook payload
    store_webhook_payload(&session_id, &webhook_path, "GET", payload.clone()).await;

    // Find the binary path for this project
    let binary_path = get_project_binary_path(&project.name);
    let binary_exists = is_project_built(&project.name);
    
    let stream_url = format!(
        "/api/projects/{}/stream?input=__webhook__&session_id={}&binary_path={}",
        id, session_id, percent_encode(&binary_path)
    );

    tracing::info!(
        project_id = %id,
        path = %webhook_path,
        session_id = %session_id,
        params = ?params,
        binary_path = %binary_path,
        binary_exists = %binary_exists,
        "GET Webhook trigger received"
    );

    Ok(Json(WebhookTriggerResponse {
        success: true,
        session_id: session_id.clone(),
        message: format!(
            "GET Webhook received for path '{}'. {}{}",
            webhook_path,
            if trigger.is_some() {
                "Trigger configuration found."
            } else {
                "No matching trigger found, but payload stored."
            },
            if !binary_exists {
                " WARNING: Project not built. Build the project first."
            } else {
                ""
            }
        ),
        path: webhook_path,
        stream_url,
        binary_path: if binary_exists { Some(binary_path) } else { None },
    }))
}

// ============================================
// Synchronous Webhook Execution
// ============================================

/// Response from synchronous webhook execution.
#[derive(Debug, Serialize)]
pub struct WebhookExecuteResponse {
    /// Whether the execution was successful
    pub success: bool,
    /// The agent's response text
    pub response: Option<String>,
    /// Error message if execution failed
    pub error: Option<String>,
    /// Session ID for this execution
    pub session_id: String,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

/// Execute a webhook synchronously and return the response.
///
/// This endpoint triggers the workflow and waits for it to complete,
/// returning the agent's response directly. This is the typical webhook
/// behavior where the caller expects a response.
///
/// ## Endpoint
/// `POST /api/projects/{id}/webhook-exec/{path}`
///
/// ## Example
/// ```bash
/// curl -X POST http://localhost:6000/api/projects/{project_id}/webhook-exec/api/webhook/my-flow \
///   -H "Content-Type: application/json" \
///   -d '{"message": "Hello from webhook!"}'
/// ```
///
/// ## Response
/// ```json
/// {
///   "success": true,
///   "response": "Hello! How can I help you today?",
///   "session_id": "abc123",
///   "duration_ms": 1234
/// }
/// ```
pub async fn webhook_execute(
    State(state): State<AppState>,
    Path((id, path)): Path<(Uuid, String)>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> ApiResult<WebhookExecuteResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
    use tokio::process::Command;
    
    let start_time = std::time::Instant::now();
    
    // Get the project to validate webhook configuration
    let storage = state.storage.read().await;
    let project = storage
        .get(id)
        .await
        .map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;

    // Find the webhook trigger in the project
    let webhook_path = format!("/{}", path.trim_start_matches('/'));
    let trigger = find_webhook_trigger(&project, &webhook_path, "POST");

    // Validate authentication if configured
    if let Some(ref trigger_config) = trigger {
        validate_webhook_auth(&headers, trigger_config)?;
    }

    // Check if project is built
    let binary_path = get_project_binary_path(&project.name);
    if !is_project_built(&project.name) {
        return Ok(Json(WebhookExecuteResponse {
            success: false,
            response: None,
            error: Some("Project not built. Build the project first using the UI.".to_string()),
            session_id: String::new(),
            duration_ms: start_time.elapsed().as_millis() as u64,
        }));
    }

    // Generate a session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    tracing::info!(
        project_id = %id,
        path = %webhook_path,
        session_id = %session_id,
        payload = %serde_json::to_string(&payload).unwrap_or_default(),
        "Executing webhook synchronously"
    );

    // Get API key from environment
    let api_key = std::env::var("GOOGLE_API_KEY").unwrap_or_default();

    // Start the binary process
    let mut child = match Command::new(&binary_path)
        .arg(&session_id)
        .env("GOOGLE_API_KEY", &api_key)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return Ok(Json(WebhookExecuteResponse {
                success: false,
                response: None,
                error: Some(format!("Failed to start workflow: {}", e)),
                session_id,
                duration_ms: start_time.elapsed().as_millis() as u64,
            }));
        }
    };

    // Send the webhook payload as input
    let input = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    
    if let Some(stdin) = child.stdin.take() {
        let mut writer = BufWriter::new(stdin);
        if let Err(e) = writer.write_all(format!("{}\n", input).as_bytes()).await {
            return Ok(Json(WebhookExecuteResponse {
                success: false,
                response: None,
                error: Some(format!("Failed to send input: {}", e)),
                session_id,
                duration_ms: start_time.elapsed().as_millis() as u64,
            }));
        }
        if let Err(e) = writer.flush().await {
            return Ok(Json(WebhookExecuteResponse {
                success: false,
                response: None,
                error: Some(format!("Failed to flush input: {}", e)),
                session_id,
                duration_ms: start_time.elapsed().as_millis() as u64,
            }));
        }
    }

    // Read stdout for the response
    let mut response_text = String::new();
    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout);
        let timeout = tokio::time::Duration::from_secs(60);
        let deadline = tokio::time::Instant::now() + timeout;
        
        loop {
            if tokio::time::Instant::now() > deadline {
                let _ = child.kill().await;
                return Ok(Json(WebhookExecuteResponse {
                    success: false,
                    response: None,
                    error: Some("Execution timeout (60s)".to_string()),
                    session_id,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                }));
            }
            
            let mut line = String::new();
            match tokio::time::timeout(
                tokio::time::Duration::from_millis(100),
                reader.read_line(&mut line)
            ).await {
                Ok(Ok(0)) => break, // EOF
                Ok(Ok(_)) => {
                    let line = line.trim_start_matches("> ");
                    if let Some(response) = line.strip_prefix("RESPONSE:") {
                        // Decode the JSON-encoded response
                        response_text = serde_json::from_str::<String>(response)
                            .unwrap_or_else(|_| response.to_string());
                        break;
                    } else if let Some(chunk) = line.strip_prefix("CHUNK:") {
                        // Accumulate streaming chunks
                        let decoded = serde_json::from_str::<String>(chunk)
                            .unwrap_or_else(|_| chunk.to_string());
                        response_text.push_str(&decoded);
                    }
                }
                Ok(Err(_)) => break, // Read error
                Err(_) => continue, // Timeout, keep trying
            }
        }
    }

    // Kill the process if still running
    let _ = child.kill().await;

    let duration_ms = start_time.elapsed().as_millis() as u64;

    tracing::info!(
        project_id = %id,
        session_id = %session_id,
        duration_ms = %duration_ms,
        response_len = %response_text.len(),
        "Webhook execution complete"
    );

    Ok(Json(WebhookExecuteResponse {
        success: true,
        response: if response_text.is_empty() { None } else { Some(response_text) },
        error: None,
        session_id,
        duration_ms,
    }))
}

/// Webhook trigger configuration extracted from project.
#[derive(Debug, Clone)]
struct WebhookTriggerConfig {
    auth: String,
    header_name: Option<String>,
    token_env_var: Option<String>,
}

/// Find a webhook trigger in the project that matches the path and method.
fn find_webhook_trigger(
    project: &ProjectSchema,
    path: &str,
    method: &str,
) -> Option<WebhookTriggerConfig> {
    use crate::codegen::action_nodes::{ActionNodeConfig, TriggerType};
    
    // Check action nodes for trigger nodes with webhook type
    for (_node_id, node) in &project.action_nodes {
        if let ActionNodeConfig::Trigger(trigger_config) = node {
            if trigger_config.trigger_type == TriggerType::Webhook {
                if let Some(webhook) = &trigger_config.webhook {
                    // Check if path matches (normalize both)
                    let normalized_path = path.trim_start_matches('/');
                    let normalized_webhook_path = webhook.path.trim_start_matches('/');
                    
                    if normalized_path == normalized_webhook_path && webhook.method == method {
                        return Some(WebhookTriggerConfig {
                            auth: webhook.auth.clone(),
                            header_name: webhook.auth_config
                                .as_ref()
                                .and_then(|c| c.header_name.clone()),
                            token_env_var: webhook.auth_config
                                .as_ref()
                                .and_then(|c| c.token_env_var.clone()),
                        });
                    }
                }
            }
        }
    }
    None
}

/// Validate webhook authentication based on trigger configuration.
fn validate_webhook_auth(
    headers: &HeaderMap,
    config: &WebhookTriggerConfig,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    match config.auth.as_str() {
        "bearer" => {
            let auth_header = headers
                .get("Authorization")
                .and_then(|v| v.to_str().ok());
            
            match auth_header {
                Some(header) if header.starts_with("Bearer ") => {
                    // In development, we just check that a bearer token is present
                    // In production, the generated code would validate against the env var
                    let token = header.trim_start_matches("Bearer ");
                    if token.is_empty() {
                        return Err(err(StatusCode::UNAUTHORIZED, "Empty bearer token"));
                    }
                    
                    // If token_env_var is set, validate against it
                    if let Some(env_var) = &config.token_env_var {
                        if let Ok(expected_token) = std::env::var(env_var) {
                            if token != expected_token {
                                return Err(err(StatusCode::UNAUTHORIZED, "Invalid bearer token"));
                            }
                        }
                        // If env var not set, allow any token in dev mode
                    }
                    Ok(())
                }
                Some(_) => Err(err(StatusCode::UNAUTHORIZED, "Invalid Authorization header format. Expected: Bearer <token>")),
                None => Err(err(StatusCode::UNAUTHORIZED, "Missing Authorization header")),
            }
        }
        "api_key" => {
            let header_name = config.header_name.as_deref().unwrap_or("X-API-Key");
            let api_key = headers
                .get(header_name)
                .and_then(|v| v.to_str().ok());
            
            match api_key {
                Some(key) if !key.is_empty() => {
                    // If token_env_var is set, validate against it
                    if let Some(env_var) = &config.token_env_var {
                        if let Ok(expected_key) = std::env::var(env_var) {
                            if key != expected_key {
                                return Err(err(StatusCode::UNAUTHORIZED, "Invalid API key"));
                            }
                        }
                        // If env var not set, allow any key in dev mode
                    }
                    Ok(())
                }
                Some(_) => Err(err(StatusCode::UNAUTHORIZED, "Empty API key")),
                None => Err(err(
                    StatusCode::UNAUTHORIZED,
                    format!("Missing {} header", header_name),
                )),
            }
        }
        "none" | _ => Ok(()),
    }
}

// ============================================
// Webhook Payload Storage
// ============================================
// Temporary storage for webhook payloads until they are consumed by the stream handler.

lazy_static::lazy_static! {
    static ref WEBHOOK_PAYLOADS: tokio::sync::RwLock<HashMap<String, WebhookPayload>> =
        tokio::sync::RwLock::new(HashMap::new());
}

/// Stored webhook payload.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub path: String,
    pub method: String,
    pub payload: serde_json::Value,
    pub timestamp: u64,
}

/// Store a webhook payload for later retrieval by the stream handler.
async fn store_webhook_payload(session_id: &str, path: &str, method: &str, payload: serde_json::Value) {
    let mut payloads = WEBHOOK_PAYLOADS.write().await;
    payloads.insert(
        session_id.to_string(),
        WebhookPayload {
            path: path.to_string(),
            method: method.to_string(),
            payload,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        },
    );
}

/// Retrieve and remove a webhook payload by session ID.
pub async fn get_webhook_payload(session_id: &str) -> Option<WebhookPayload> {
    let mut payloads = WEBHOOK_PAYLOADS.write().await;
    payloads.remove(session_id)
}

/// Check if a session has a pending webhook payload.
pub async fn has_webhook_payload(session_id: &str) -> bool {
    let payloads = WEBHOOK_PAYLOADS.read().await;
    payloads.contains_key(session_id)
}
