use adk_core::{Part, Result};
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct SaveRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
    pub part: Part,
    pub version: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SaveResponse {
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct LoadRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
    pub version: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct LoadResponse {
    pub part: Part,
}

#[derive(Debug, Clone)]
pub struct DeleteRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
    pub version: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ListRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub struct ListResponse {
    pub file_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct VersionsRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub file_name: String,
}

#[derive(Debug, Clone)]
pub struct VersionsResponse {
    pub versions: Vec<i64>,
}

#[async_trait]
pub trait ArtifactService: Send + Sync {
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse>;
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse>;
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    async fn list(&self, req: ListRequest) -> Result<ListResponse>;
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse>;
}
