use crate::server::{handlers, sse, state::AppState};
use axum::{
    Router,
    routing::{delete, get, post, put},
};

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/projects", get(handlers::list_projects))
        .route("/projects", post(handlers::create_project))
        .route("/projects/:id", get(handlers::get_project))
        .route("/projects/:id", put(handlers::update_project))
        .route("/projects/:id", delete(handlers::delete_project))
        .route("/projects/:id/run", post(handlers::run_project))
        .route("/projects/:id/stream", get(sse::stream_handler))
        .route("/projects/:id/session", delete(handlers::clear_session))
        .route("/projects/:id/compile", get(handlers::compile_project))
        .route("/projects/:id/deploy", post(handlers::deploy_project))
        .route("/projects/:id/build", post(handlers::build_project))
        .route("/projects/:id/build-stream", get(handlers::build_project_stream))
        .route("/sessions/:session_id", delete(sse::kill_session))
        // Task 10: HITL Resume Endpoint
        // POST /api/sessions/{session_id}/resume - Resume an interrupted workflow
        // Requirements: 3.2, 5.2
        .route("/sessions/:session_id/resume", post(handlers::resume_session))
        // Webhook trigger endpoints for testing webhooks in development
        // POST /api/projects/{id}/webhook/*path - Trigger workflow via webhook (async, returns stream URL)
        // GET /api/projects/{id}/webhook/*path - Trigger workflow via webhook GET (async)
        .route("/projects/:id/webhook/*path", post(handlers::webhook_trigger))
        .route("/projects/:id/webhook/*path", get(handlers::webhook_trigger_get))
        // Synchronous webhook execution - waits for response
        // POST /api/projects/{id}/webhook-exec/*path - Execute workflow and return response
        .route("/projects/:id/webhook-exec/*path", post(handlers::webhook_execute))
        // Webhook notification SSE endpoint - UI subscribes to receive webhook events
        // GET /api/projects/{id}/webhook-events - SSE stream of webhook notifications
        .route("/projects/:id/webhook-events", get(sse::webhook_events_handler))
        // Event trigger endpoint - external systems send events to trigger workflows
        // POST /api/projects/{id}/events - Trigger workflow via event (matches source + eventType)
        .route("/projects/:id/events", post(handlers::event_trigger))
        // API Key Management endpoints
        .route("/settings/detected-keys", get(handlers::detected_keys))
        .route("/projects/:id/keys", get(handlers::get_project_keys))
        .route("/projects/:id/keys", post(handlers::save_project_keys))
        .route("/projects/:id/keys/:key_name", delete(handlers::delete_project_key))
}
