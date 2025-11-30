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
}
