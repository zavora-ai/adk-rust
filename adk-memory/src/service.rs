use adk_core::{Content, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub content: Content,
    pub author: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub user_id: String,
    pub app_name: String,
}

#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub memories: Vec<MemoryEntry>,
}

#[async_trait]
pub trait MemoryService: Send + Sync {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()>;
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;
}
