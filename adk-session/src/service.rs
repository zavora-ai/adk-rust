use crate::{Event, Session};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CreateRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: Option<String>,
    pub state: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct GetRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub num_recent_events: Option<usize>,
    pub after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ListRequest {
    pub app_name: String,
    pub user_id: String,
    /// Maximum number of sessions to return. `None` means no limit.
    pub limit: Option<usize>,
    /// Number of sessions to skip for pagination. `None` means start from the beginning.
    pub offset: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct DeleteRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
}

#[async_trait]
pub trait SessionService: Send + Sync {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    async fn append_event(&self, session_id: &str, event: Event) -> Result<()>;

    /// Delete all sessions for a given app and user.
    ///
    /// Removes all sessions and their associated events. Useful for
    /// bulk cleanup and GDPR right-to-erasure compliance.
    /// The default implementation returns an error.
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let _ = (app_name, user_id);
        Err(adk_core::AdkError::Session("delete_all_sessions not implemented".into()))
    }

    /// Verify backend connectivity.
    ///
    /// Returns `Ok(())` if the backend is reachable and responsive.
    /// Use this for Kubernetes readiness probes and `/healthz` endpoints.
    /// The default implementation always succeeds (suitable for in-memory).
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}
