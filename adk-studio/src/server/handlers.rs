use crate::schema::{ProjectMeta, ProjectSchema};
use crate::server::state::AppState;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
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
