use crate::schema::{ProjectMeta, ProjectSchema};
use crate::server::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
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
    storage.list().await.map(Json).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
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

/// Run project request
#[derive(Deserialize)]
pub struct RunRequest {
    pub input: String,
}

/// Run project response
#[derive(Serialize)]
pub struct RunResponse {
    pub output: String,
}

/// Run a project with input
pub async fn run_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<RunRequest>,
) -> ApiResult<RunResponse> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| err(StatusCode::BAD_REQUEST, "GOOGLE_API_KEY not set"))?;

    let storage = state.storage.read().await;
    let project = storage.get(id).await.map_err(|e| err(StatusCode::NOT_FOUND, e.to_string()))?;

    let output = crate::runtime::run_project(&project, &req.input, &api_key)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(RunResponse { output }))
}
