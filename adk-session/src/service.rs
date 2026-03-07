use crate::{Event, Session};
use adk_core::{
    Result,
    types::{SessionId, UserId},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CreateRequest {
    pub app_name: String,
    pub user_id: UserId,
    pub session_id: Option<SessionId>,
    pub state: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct GetRequest {
    pub app_name: String,
    pub user_id: UserId,
    pub session_id: SessionId,
    pub num_recent_events: Option<usize>,
    pub after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ListRequest {
    pub app_name: String,
    pub user_id: UserId,
}

#[derive(Debug, Clone)]
pub struct DeleteRequest {
    pub app_name: String,
    pub user_id: UserId,
    pub session_id: SessionId,
}

#[async_trait]
pub trait SessionService: Send + Sync {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    async fn append_event(&self, session_id: &SessionId, event: Event) -> Result<()>;
}
