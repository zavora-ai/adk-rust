use std::{env, net::SocketAddr, sync::Arc};

use crate::types::{
    AgentActionResponse, AgentScaleRequest, AlertEvent, AlertRule, ApiExplorerResponse,
    AuditExportResponse, BillingUsageItem, CatalogDeployRequest, CatalogTemplate,
    ChangeTierRequest, CreateAlertRuleRequest, CreateApiKeyRequest, CreateApiKeyResponse,
    CreateEnvironmentRequest, EvaluationRun, EvaluationsResponse, HitlDecisionRequest,
    InviteTeamMemberRequest, PromoteEnvironmentRequest, TeamMember, WorkspaceResponse,
};
use adk_deploy::{
    AgentDetail, AlertSummary, AuditEvent, AuthSessionResponse, BillingSummary, DashboardResponse,
    DeploymentHistoryResponse, DeploymentStatusResponse, EnvironmentSummary, HitlCheckpoint,
    LogEntry, LoginRequest, LoginResponse, PushDeploymentRequest, PushDeploymentResponse,
    SecretListResponse, SecretSetRequest, TraceSummary,
};
use axum::{
    Extension, Json, Router,
    extract::Request,
    extract::{Multipart, Path, Query, State},
    http::header,
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};

mod auth;
mod state;
mod types;

use crate::auth::{AuthContext, AuthService};
pub use state::PlatformState;

#[derive(Clone)]
pub struct AppState {
    platform: Arc<PlatformState>,
    auth: Arc<AuthService>,
}

impl AppState {
    pub async fn load() -> Result<Self, String> {
        let platform = Arc::new(PlatformState::load().await.map_err(|e| e.to_string())?);
        let auth = Arc::new(AuthService::from_env()?);
        Ok(Self { platform, auth })
    }

    #[cfg(test)]
    pub(crate) fn from_platform(platform: PlatformState) -> Self {
        Self { platform: Arc::new(platform), auth: Arc::new(AuthService::test_default()) }
    }
}

pub fn create_app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/dashboard", get(dashboard))
        .route("/agents", get(list_agents))
        .route("/agents/{agent_name}", get(agent_detail))
        .route("/agents/{agent_name}/restart", post(restart_agent))
        .route("/agents/{agent_name}/scale", post(scale_agent))
        .route("/environments", get(list_environments).post(create_environment))
        .route("/environments/{environment}/promote", post(promote_environment_between_envs))
        .route("/traces", get(list_traces))
        .route("/logs", get(list_logs))
        .route("/evaluations", get(list_evaluations).post(run_evaluation))
        .route("/catalog", get(list_catalog))
        .route("/catalog/{template_id}/deploy", post(deploy_catalog_template))
        .route("/hitl", get(list_hitl))
        .route("/hitl/{checkpoint_id}/approve", post(approve_hitl))
        .route("/hitl/{checkpoint_id}/reject", post(reject_hitl))
        .route("/alerts", get(list_alerts))
        .route("/alerts/rules", get(list_alert_rules).post(create_alert_rule))
        .route("/alerts/history", get(list_alert_history))
        .route("/alerts/rules/{rule_id}/suppress", post(suppress_alert_rule))
        .route("/billing", get(list_billing))
        .route("/billing/usage", get(list_billing_usage))
        .route("/billing/tier", post(change_billing_tier))
        .route("/audit", get(list_audit))
        .route("/audit/export", post(export_audit))
        .route("/team", get(list_team).post(invite_team_member))
        .route("/team/{member_id}", delete(remove_team_member))
        .route("/api-explorer", get(api_explorer))
        .route("/api-keys", post(create_api_key))
        .route("/api-keys/{key_id}", delete(delete_api_key))
        .route("/openapi.json", get(openapi_spec))
        .route("/auth/session", get(auth_session))
        .route("/deployments", post(push_deployment))
        .route("/deployments/status", get(deployment_status))
        .route("/deployments/history", get(deployment_history))
        .route("/deployments/{deployment_id}/rollback", post(rollback))
        .route("/deployments/{deployment_id}/promote", post(promote))
        .route("/secrets", post(set_secret).get(list_secrets))
        .route("/secrets/{key}", delete(delete_secret))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/health", get(health))
        .route("/api/v1/auth/login", post(login))
        .nest("/api/v1", protected)
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST, Method::DELETE])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
                .allow_origin(allowed_origins()),
        )
        .layer(TraceLayer::new_for_http())
}

pub async fn serve(addr: SocketAddr) -> Result<(), String> {
    let state = AppState::load().await?;
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| e.to_string())?;
    axum::serve(listener, create_app(state)).await.map_err(|e| e.to_string())
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, String)> {
    if !state.auth.dev_login_enabled() {
        return Err((
            StatusCode::FORBIDDEN,
            "developer login is disabled; provide a bearer token via external auth or bootstrap token"
                .to_string(),
        ));
    }
    state.platform.login(request).await.map(Json).map_err(internal_error)
}

async fn auth_session(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<AuthSessionResponse>, (StatusCode, String)> {
    let workspace =
        state.platform.workspace_summary(&context.workspace_id).await.map_err(internal_error)?;
    Ok(Json(AuthSessionResponse {
        user_id: context.user_id,
        workspace_id: workspace.id,
        workspace_name: workspace.name,
        scopes: context.scopes,
    }))
}

async fn dashboard(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
) -> Result<Json<DashboardResponse>, (StatusCode, String)> {
    state
        .platform
        .dashboard_for_workspace(&context.workspace_id)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn list_environments(
    State(state): State<AppState>,
) -> Result<Json<Vec<EnvironmentSummary>>, (StatusCode, String)> {
    state.platform.list_environments().await.map(Json).map_err(internal_error)
}

async fn list_traces(
    State(state): State<AppState>,
) -> Result<Json<Vec<TraceSummary>>, (StatusCode, String)> {
    state.platform.traces().await.map(Json).map_err(internal_error)
}

async fn list_logs(
    State(state): State<AppState>,
) -> Result<Json<Vec<LogEntry>>, (StatusCode, String)> {
    state.platform.logs().await.map(Json).map_err(internal_error)
}

async fn list_hitl(
    State(state): State<AppState>,
) -> Result<Json<Vec<HitlCheckpoint>>, (StatusCode, String)> {
    state.platform.hitl().await.map(Json).map_err(internal_error)
}

async fn approve_hitl(
    State(state): State<AppState>,
    Path(checkpoint_id): Path<String>,
    Json(request): Json<HitlDecisionRequest>,
) -> Result<Json<HitlCheckpoint>, (StatusCode, String)> {
    state
        .platform
        .approve_hitl(&checkpoint_id, request)
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn reject_hitl(
    State(state): State<AppState>,
    Path(checkpoint_id): Path<String>,
    Json(request): Json<HitlDecisionRequest>,
) -> Result<Json<HitlCheckpoint>, (StatusCode, String)> {
    state
        .platform
        .reject_hitl(&checkpoint_id, request)
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn list_alerts(
    State(state): State<AppState>,
) -> Result<Json<Vec<AlertSummary>>, (StatusCode, String)> {
    state.platform.alerts().await.map(Json).map_err(internal_error)
}

async fn list_billing(
    State(state): State<AppState>,
) -> Result<Json<Vec<BillingSummary>>, (StatusCode, String)> {
    state.platform.billing().await.map(Json).map_err(internal_error)
}

async fn list_billing_usage(
    State(state): State<AppState>,
) -> Result<Json<Vec<BillingUsageItem>>, (StatusCode, String)> {
    state.platform.billing_usage().await.map(Json).map_err(internal_error)
}

async fn change_billing_tier(
    State(state): State<AppState>,
    Json(request): Json<ChangeTierRequest>,
) -> Result<Json<WorkspaceResponse>, (StatusCode, String)> {
    state.platform.change_tier(request).await.map(Json).map_err(internal_error)
}

async fn list_audit(
    State(state): State<AppState>,
) -> Result<Json<Vec<AuditEvent>>, (StatusCode, String)> {
    state.platform.audit_events().await.map(Json).map_err(internal_error)
}

async fn export_audit(
    State(state): State<AppState>,
) -> Result<Json<AuditExportResponse>, (StatusCode, String)> {
    state.platform.audit_export().await.map(Json).map_err(internal_error)
}

async fn list_evaluations(
    State(state): State<AppState>,
) -> Result<Json<EvaluationsResponse>, (StatusCode, String)> {
    state.platform.evaluations().await.map(Json).map_err(internal_error)
}

async fn run_evaluation(
    State(state): State<AppState>,
    Json(request): Json<crate::types::RunEvaluationRequest>,
) -> Result<Json<EvaluationRun>, (StatusCode, String)> {
    state.platform.run_evaluation(request).await.map(Json).map_err(internal_error)
}

async fn list_catalog(
    State(state): State<AppState>,
) -> Result<Json<Vec<CatalogTemplate>>, (StatusCode, String)> {
    state.platform.catalog().await.map(Json).map_err(internal_error)
}

async fn deploy_catalog_template(
    State(state): State<AppState>,
    Path(template_id): Path<String>,
    Json(request): Json<CatalogDeployRequest>,
) -> Result<Json<adk_deploy::DeploymentRecord>, (StatusCode, String)> {
    state
        .platform
        .deploy_template(&template_id, &request.environment, request.workspace_id)
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn list_team(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamMember>>, (StatusCode, String)> {
    state.platform.list_team_members().await.map(Json).map_err(internal_error)
}

async fn invite_team_member(
    State(state): State<AppState>,
    Json(request): Json<InviteTeamMemberRequest>,
) -> Result<Json<TeamMember>, (StatusCode, String)> {
    state.platform.invite_team_member(request).await.map(Json).map_err(internal_error)
}

async fn remove_team_member(
    State(state): State<AppState>,
    Path(member_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .platform
        .remove_team_member(&member_id)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(not_found_or_internal)
}

async fn api_explorer(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiExplorerResponse>, (StatusCode, String)> {
    state.platform.api_explorer(&api_base_url(&headers)).await.map(Json).map_err(internal_error)
}

async fn create_api_key(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Json(request): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>, (StatusCode, String)> {
    state
        .platform
        .create_api_key_for_workspace(&context.workspace_id, request)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn delete_api_key(
    State(state): State<AppState>,
    Path(key_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .platform
        .delete_api_key(&key_id)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(not_found_or_internal)
}

async fn openapi_spec(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state.platform.openapi_spec(&api_base_url(&headers)).await.map(Json).map_err(internal_error)
}

async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Vec<adk_deploy::AgentSummary>>, (StatusCode, String)> {
    state.platform.list_agents().await.map(Json).map_err(internal_error)
}

#[derive(Debug, Deserialize)]
struct AgentQuery {
    environment: String,
}

async fn agent_detail(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Path(agent_name): Path<String>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<AgentDetail>, (StatusCode, String)> {
    state
        .platform
        .agent_detail_for_workspace(&context.workspace_id, &agent_name, &query.environment)
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn restart_agent(
    State(state): State<AppState>,
    Path(agent_name): Path<String>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<AgentActionResponse>, (StatusCode, String)> {
    state
        .platform
        .restart_agent(&query.environment, &agent_name)
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn scale_agent(
    State(state): State<AppState>,
    Path(agent_name): Path<String>,
    Query(query): Query<AgentQuery>,
    Json(request): Json<AgentScaleRequest>,
) -> Result<Json<AgentActionResponse>, (StatusCode, String)> {
    state
        .platform
        .scale_agent(&query.environment, &agent_name, request)
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn push_deployment(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    multipart: Multipart,
) -> Result<Json<PushDeploymentResponse>, (StatusCode, String)> {
    let (mut request, bundle_name, bundle_bytes) =
        parse_push_deployment_multipart(multipart).await?;
    request.workspace_id = Some(context.workspace_id);
    state
        .platform
        .push_uploaded_deployment(request, bundle_name.as_deref(), &bundle_bytes)
        .await
        .map(|deployment| Json(PushDeploymentResponse { deployment }))
        .map_err(internal_error)
}

#[derive(Debug, Deserialize)]
struct DeploymentQuery {
    environment: String,
    agent: Option<String>,
}

async fn deployment_status(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<DeploymentQuery>,
) -> Result<Json<DeploymentStatusResponse>, (StatusCode, String)> {
    state
        .platform
        .deployment_status_for_workspace(
            &context.workspace_id,
            &query.environment,
            query.agent.as_deref(),
        )
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn deployment_history(
    State(state): State<AppState>,
    Extension(context): Extension<AuthContext>,
    Query(query): Query<DeploymentQuery>,
) -> Result<Json<DeploymentHistoryResponse>, (StatusCode, String)> {
    state
        .platform
        .deployment_history_for_workspace(
            &context.workspace_id,
            &query.environment,
            query.agent.as_deref(),
        )
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn rollback(
    State(state): State<AppState>,
    Path(deployment_id): Path<String>,
) -> Result<Json<DeploymentStatusResponse>, (StatusCode, String)> {
    state.platform.rollback(&deployment_id).await.map(Json).map_err(not_found_or_internal)
}

async fn promote(
    State(state): State<AppState>,
    Path(deployment_id): Path<String>,
) -> Result<Json<DeploymentStatusResponse>, (StatusCode, String)> {
    state.platform.promote(&deployment_id).await.map(Json).map_err(not_found_or_internal)
}

async fn set_secret(
    State(state): State<AppState>,
    Json(request): Json<SecretSetRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .platform
        .set_secret(request)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(internal_error)
}

#[derive(Debug, Deserialize)]
struct SecretQuery {
    environment: String,
}

async fn list_secrets(
    State(state): State<AppState>,
    Query(query): Query<SecretQuery>,
) -> Result<Json<SecretListResponse>, (StatusCode, String)> {
    state.platform.list_secrets(&query.environment).await.map(Json).map_err(internal_error)
}

async fn create_environment(
    State(state): State<AppState>,
    Json(request): Json<CreateEnvironmentRequest>,
) -> Result<Json<EnvironmentSummary>, (StatusCode, String)> {
    state.platform.create_environment(request).await.map(Json).map_err(internal_error)
}

async fn promote_environment_between_envs(
    State(state): State<AppState>,
    Path(environment): Path<String>,
    Json(request): Json<PromoteEnvironmentRequest>,
) -> Result<Json<adk_deploy::DeploymentRecord>, (StatusCode, String)> {
    state
        .platform
        .promote_environment(&environment, request)
        .await
        .map(Json)
        .map_err(not_found_or_internal)
}

async fn list_alert_rules(
    State(state): State<AppState>,
) -> Result<Json<Vec<AlertRule>>, (StatusCode, String)> {
    state.platform.alert_rules().await.map(Json).map_err(internal_error)
}

async fn create_alert_rule(
    State(state): State<AppState>,
    Json(request): Json<CreateAlertRuleRequest>,
) -> Result<Json<AlertRule>, (StatusCode, String)> {
    state.platform.create_alert_rule(request).await.map(Json).map_err(internal_error)
}

async fn list_alert_history(
    State(state): State<AppState>,
) -> Result<Json<Vec<AlertEvent>>, (StatusCode, String)> {
    state.platform.alert_history().await.map(Json).map_err(internal_error)
}

async fn suppress_alert_rule(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
) -> Result<Json<AlertRule>, (StatusCode, String)> {
    state.platform.suppress_alert_rule(&rule_id).await.map(Json).map_err(not_found_or_internal)
}

async fn delete_secret(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<SecretQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .platform
        .delete_secret(&query.environment, &key)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(internal_error)
}

async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let default_workspace_id = state
        .platform
        .default_workspace_id()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let context = match state.auth.authenticate(authorization, &default_workspace_id).await {
        Ok(Some(context)) => context,
        Ok(None) | Err(_) => {
            let token = state
                .platform
                .authorize_token(authorization)
                .await
                .map_err(|_| StatusCode::UNAUTHORIZED)?;
            AuthContext {
                user_id: token.principal,
                workspace_id: token.workspace_id,
                scopes: token.scopes,
            }
        }
    };
    let mut request = request;
    request.extensions_mut().insert(context);
    Ok(next.run(request).await)
}

fn api_base_url(headers: &HeaderMap) -> String {
    let scheme =
        headers.get("x-forwarded-proto").and_then(|value| value.to_str().ok()).unwrap_or("http");
    let host =
        headers.get(header::HOST).and_then(|value| value.to_str().ok()).unwrap_or("127.0.0.1:8090");
    format!("{scheme}://{host}/api/v1")
}

fn allowed_origins() -> AllowOrigin {
    let configured = env::var("ADK_DEPLOY_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://127.0.0.1:8091,http://localhost:8091".to_string());
    let mut values = configured
        .split(',')
        .filter_map(|origin| HeaderValue::from_str(origin.trim()).ok())
        .collect::<Vec<_>>();
    if values.is_empty() {
        values.push(HeaderValue::from_static("http://127.0.0.1:8091"));
    }
    AllowOrigin::list(values)
}

fn internal_error(error: String) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error)
}

async fn parse_push_deployment_multipart(
    mut multipart: Multipart,
) -> Result<(PushDeploymentRequest, Option<String>, Vec<u8>), (StatusCode, String)> {
    let mut request = None;
    let mut bundle_name = None;
    let mut bundle_bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(bad_request)? {
        let field_name = field.name().map(str::to_string);
        match field_name.as_deref() {
            Some("request") => {
                let payload = field.text().await.map_err(bad_request)?;
                request = Some(
                    serde_json::from_str::<PushDeploymentRequest>(&payload)
                        .map_err(|error| bad_request(error.to_string()))?,
                );
            }
            Some("bundle") => {
                bundle_name = field.file_name().map(str::to_string);
                bundle_bytes = Some(field.bytes().await.map_err(bad_request)?.to_vec());
            }
            _ => {}
        }
    }

    let request =
        request.ok_or_else(|| bad_request("multipart field `request` is required".to_string()))?;
    let bundle_bytes = bundle_bytes
        .ok_or_else(|| bad_request("multipart field `bundle` is required".to_string()))?;
    Ok((request, bundle_name, bundle_bytes))
}

fn bad_request(error: impl ToString) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, error.to_string())
}

fn not_found_or_internal(error: String) -> (StatusCode, String) {
    if error.contains("not found") {
        (StatusCode::NOT_FOUND, error)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, error)
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, create_app};
    use crate::PlatformState;
    use adk_deploy::{DeploymentManifest, LoginResponse, PushDeploymentRequest, SourceInfo};
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode, header},
    };
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn dashboard_requires_authentication() {
        let dir = tempdir().unwrap();
        let platform = PlatformState::load_from_dir(dir.path().to_path_buf()).await.unwrap();
        let app = create_app(AppState::from_platform(platform));

        let response = app
            .oneshot(Request::builder().uri("/api/v1/dashboard").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_session_returns_workspace_for_static_token() {
        let dir = tempdir().unwrap();
        let platform = PlatformState::load_from_dir(dir.path().to_path_buf()).await.unwrap();
        let app = create_app(AppState::from_platform(platform));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/auth/session")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let payload = read_json::<serde_json::Value>(response).await;
        assert_eq!(payload["userId"], "test-operator");
        assert_eq!(payload["workspaceId"], "ws_default");
    }

    #[tokio::test]
    async fn login_then_push_and_query_deployment() {
        let dir = tempdir().unwrap();
        let platform = PlatformState::load_from_dir(dir.path().to_path_buf()).await.unwrap();
        let app = create_app(AppState::from_platform(platform));

        let login_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/auth/login",
                serde_json::json!({
                    "email": "operator@example.com",
                    "workspaceName": "Default Workspace"
                }),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(login_response.status(), StatusCode::OK);

        let login: LoginResponse = read_json(login_response).await;
        let mut manifest = DeploymentManifest::default();
        manifest.agent.name = "studio-agent".to_string();
        manifest.agent.binary = "studio-agent".to_string();
        manifest.source = Some(SourceInfo {
            kind: "adk_studio".to_string(),
            project_id: Some("project_123".to_string()),
            project_name: Some("Studio Agent".to_string()),
        });

        let push_response = app
            .clone()
            .oneshot(multipart_push_request(
                Method::POST,
                "/api/v1/deployments",
                &PushDeploymentRequest {
                    workspace_id: Some(login.workspace_id.clone()),
                    environment: "staging".to_string(),
                    manifest,
                    bundle_path: "/tmp/studio-agent.tar.gz".to_string(),
                    checksum_sha256: format!("{:x}", Sha256::digest(b"studio-bundle")),
                    binary_path: Some("/tmp/studio-agent".to_string()),
                },
                b"studio-bundle",
                Some(&login.token),
            ))
            .await
            .unwrap();
        assert_eq!(push_response.status(), StatusCode::OK);

        let status_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/deployments/status?environment=staging&agent=studio-agent")
                    .header(header::AUTHORIZATION, format!("Bearer {}", login.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status_response.status(), StatusCode::OK);
        let payload = read_json::<serde_json::Value>(status_response).await;
        assert_eq!(payload["deployment"]["agentName"], "studio-agent");
        assert_eq!(payload["deployment"]["environment"], "staging");

        let history_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/deployments/history?environment=staging&agent=studio-agent")
                    .header(header::AUTHORIZATION, format!("Bearer {}", login.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(history_response.status(), StatusCode::OK);
        let history = read_json::<serde_json::Value>(history_response).await;
        assert_eq!(history["items"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn api_key_and_console_routes_work_end_to_end() {
        let dir = tempdir().unwrap();
        let platform = PlatformState::load_from_dir(dir.path().to_path_buf()).await.unwrap();
        let app = create_app(AppState::from_platform(platform));

        let login_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/auth/login",
                serde_json::json!({
                    "email": "operator@example.com",
                    "workspaceName": "Default Workspace"
                }),
                None,
            ))
            .await
            .unwrap();
        let login: LoginResponse = read_json(login_response).await;

        let key_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/api-keys",
                serde_json::json!({
                    "name": "Console key",
                    "scopes": ["deploy:read", "deploy:write"]
                }),
                Some(&login.token),
            ))
            .await
            .unwrap();
        assert_eq!(key_response.status(), StatusCode::OK);
        let created_key = read_json::<serde_json::Value>(key_response).await;
        let api_key_token = created_key["token"].as_str().unwrap();

        let dashboard_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/dashboard")
                    .header(header::AUTHORIZATION, format!("Bearer {api_key_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dashboard_response.status(), StatusCode::OK);

        let evaluation_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/evaluations",
                serde_json::json!({
                    "agent": "job-hunter-super-agent",
                    "environment": "production",
                    "dataset": "production-golden"
                }),
                Some(&login.token),
            ))
            .await
            .unwrap();
        assert_eq!(evaluation_response.status(), StatusCode::OK);

        let invite_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/team",
                serde_json::json!({
                    "email": "new.operator@example.com",
                    "role": "runtime_engineer"
                }),
                Some(&login.token),
            ))
            .await
            .unwrap();
        assert_eq!(invite_response.status(), StatusCode::OK);

        let environment_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/environments",
                serde_json::json!({
                    "name": "sandbox",
                    "region": "US"
                }),
                Some(&login.token),
            ))
            .await
            .unwrap();
        assert_eq!(environment_response.status(), StatusCode::OK);

        let alert_rule_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/alerts/rules",
                serde_json::json!({
                    "name": "Budget burn",
                    "condition": "spend above 80 percent",
                    "channel": "email"
                }),
                Some(&login.token),
            ))
            .await
            .unwrap();
        assert_eq!(alert_rule_response.status(), StatusCode::OK);

        let hitl_response = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/hitl/cp_1800/approve",
                serde_json::json!({
                    "reviewer": "operator@example.com"
                }),
                Some(&login.token),
            ))
            .await
            .unwrap();
        assert_eq!(hitl_response.status(), StatusCode::OK);
        let hitl_payload = read_json::<serde_json::Value>(hitl_response).await;
        assert_eq!(hitl_payload["state"], "Approved");

        let docs_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/api-explorer")
                    .header(header::HOST, "127.0.0.1:8090")
                    .header(header::AUTHORIZATION, format!("Bearer {}", login.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(docs_response.status(), StatusCode::OK);
        let docs_payload = read_json::<serde_json::Value>(docs_response).await;
        assert!(docs_payload["endpoints"].as_array().unwrap().len() >= 6);
    }

    fn json_request<T: serde::Serialize>(
        method: Method,
        uri: &str,
        payload: T,
        token: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::from(serde_json::to_vec(&payload).unwrap())).unwrap()
    }

    fn multipart_push_request(
        method: Method,
        uri: &str,
        payload: &PushDeploymentRequest,
        bundle: &[u8],
        token: Option<&str>,
    ) -> Request<Body> {
        let boundary = "adk-boundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"request\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"bundle\"; filename=\"bundle.tar.gz\"\r\nContent-Type: application/gzip\r\n\r\n",
            serde_json::to_string(payload).unwrap()
        )
        .into_bytes();
        body.extend_from_slice(bundle);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"));
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::from(body)).unwrap()
    }

    async fn read_json<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}
