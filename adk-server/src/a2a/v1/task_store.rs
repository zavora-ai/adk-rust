//! A2A v1.0.0 task store — trait and in-memory implementation.
//!
//! Defines [`TaskStore`] for persisting A2A task state (status, artifacts,
//! history) beyond a single execution, and [`InMemoryTaskStore`] for
//! development and testing.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;

use super::error::A2aError;

/// An entry in the task store representing a persisted A2A task.
#[derive(Debug, Clone)]
pub struct TaskStoreEntry {
    /// Unique task identifier.
    pub id: String,
    /// Conversation context this task belongs to.
    pub context_id: String,
    /// Current task status (state + optional message + timestamp).
    pub status: a2a_protocol_types::TaskStatus,
    /// Artifacts produced by this task.
    pub artifacts: Vec<a2a_protocol_types::Artifact>,
    /// Historical messages exchanged during this task.
    pub history: Vec<a2a_protocol_types::Message>,
    /// Arbitrary metadata.
    pub metadata: HashMap<String, serde_json::Value>,
    /// Push notification configurations for this task.
    pub push_configs: Vec<a2a_protocol_types::TaskPushNotificationConfig>,
    /// Timestamp when the task was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Timestamp when the task was last updated.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Parameters for listing tasks with optional filtering and pagination.
#[derive(Debug, Clone, Default)]
pub struct ListTasksParams {
    /// Filter by conversation context ID.
    pub context_id: Option<String>,
    /// Filter by task state.
    pub state: Option<a2a_protocol_types::TaskState>,
    /// Maximum number of tasks to return.
    pub page_size: Option<u32>,
    /// Opaque cursor for pagination.
    pub page_token: Option<String>,
    /// Maximum number of history messages to include per task.
    pub history_length: Option<u32>,
    /// Filter tasks with status timestamp after this ISO 8601 value.
    pub status_timestamp_after: Option<String>,
    /// Whether to include artifacts in the response.
    pub include_artifacts: Option<bool>,
}

/// Async trait for persisting A2A task state.
///
/// Implementations must be `Send + Sync` for use across async boundaries.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Persists a new task entry.
    async fn create_task(&self, entry: TaskStoreEntry) -> Result<(), A2aError>;

    /// Retrieves a task by ID.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    async fn get_task(&self, task_id: &str) -> Result<TaskStoreEntry, A2aError>;

    /// Updates the status of an existing task.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    async fn update_status(
        &self,
        task_id: &str,
        status: a2a_protocol_types::TaskStatus,
    ) -> Result<(), A2aError>;

    /// Appends an artifact to an existing task.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    async fn add_artifact(
        &self,
        task_id: &str,
        artifact: a2a_protocol_types::Artifact,
    ) -> Result<(), A2aError>;

    /// Appends a message to the task's history.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    async fn add_history_message(
        &self,
        task_id: &str,
        message: a2a_protocol_types::Message,
    ) -> Result<(), A2aError>;

    /// Finds the most recently updated non-terminal task for a given contextId.
    /// Returns None if no non-terminal task exists for the context.
    async fn find_task_by_context(
        &self,
        context_id: &str,
    ) -> Result<Option<TaskStoreEntry>, A2aError>;

    /// Lists tasks matching the given parameters.
    async fn list_tasks(&self, params: ListTasksParams) -> Result<Vec<TaskStoreEntry>, A2aError>;

    /// Deletes a task by ID.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    async fn delete_task(&self, task_id: &str) -> Result<(), A2aError>;
}

/// In-memory task store for development and testing.
///
/// Uses a `RwLock<HashMap>` for concurrent access. Not suitable for
/// production deployments that require persistence across restarts.
pub struct InMemoryTaskStore {
    tasks: RwLock<HashMap<String, TaskStoreEntry>>,
}

impl InMemoryTaskStore {
    /// Creates a new empty in-memory task store.
    #[must_use]
    pub fn new() -> Self {
        Self { tasks: RwLock::new(HashMap::new()) }
    }
}

impl Default for InMemoryTaskStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn create_task(&self, entry: TaskStoreEntry) -> Result<(), A2aError> {
        let mut tasks = self.tasks.write().await;
        tasks.insert(entry.id.clone(), entry);
        Ok(())
    }

    async fn get_task(&self, task_id: &str) -> Result<TaskStoreEntry, A2aError> {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| A2aError::TaskNotFound { task_id: task_id.to_string() })
    }

    async fn update_status(
        &self,
        task_id: &str,
        status: a2a_protocol_types::TaskStatus,
    ) -> Result<(), A2aError> {
        let mut tasks = self.tasks.write().await;
        let entry = tasks
            .get_mut(task_id)
            .ok_or_else(|| A2aError::TaskNotFound { task_id: task_id.to_string() })?;
        entry.status = status;
        entry.updated_at = Utc::now();
        Ok(())
    }

    async fn add_artifact(
        &self,
        task_id: &str,
        artifact: a2a_protocol_types::Artifact,
    ) -> Result<(), A2aError> {
        let mut tasks = self.tasks.write().await;
        let entry = tasks
            .get_mut(task_id)
            .ok_or_else(|| A2aError::TaskNotFound { task_id: task_id.to_string() })?;
        entry.artifacts.push(artifact);
        entry.updated_at = Utc::now();
        Ok(())
    }

    async fn add_history_message(
        &self,
        task_id: &str,
        message: a2a_protocol_types::Message,
    ) -> Result<(), A2aError> {
        let mut tasks = self.tasks.write().await;
        let entry = tasks
            .get_mut(task_id)
            .ok_or_else(|| A2aError::TaskNotFound { task_id: task_id.to_string() })?;
        entry.history.push(message);
        entry.updated_at = Utc::now();
        Ok(())
    }

    async fn find_task_by_context(
        &self,
        context_id: &str,
    ) -> Result<Option<TaskStoreEntry>, A2aError> {
        let tasks = self.tasks.read().await;
        let result = tasks
            .values()
            .filter(|entry| {
                entry.context_id == context_id
                    && !matches!(
                        entry.status.state,
                        a2a_protocol_types::TaskState::Completed
                            | a2a_protocol_types::TaskState::Failed
                            | a2a_protocol_types::TaskState::Canceled
                            | a2a_protocol_types::TaskState::Rejected
                    )
            })
            .max_by_key(|entry| entry.updated_at)
            .cloned();
        Ok(result)
    }

    async fn list_tasks(&self, params: ListTasksParams) -> Result<Vec<TaskStoreEntry>, A2aError> {
        let tasks = self.tasks.read().await;
        let mut results: Vec<TaskStoreEntry> = tasks
            .values()
            .filter(|entry| {
                if let Some(ref ctx) = params.context_id {
                    if entry.context_id != *ctx {
                        return false;
                    }
                }
                if let Some(ref state) = params.state {
                    if entry.status.state != *state {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        // Sort by created_at for deterministic pagination
        results.sort_by(|a, b| a.created_at.cmp(&b.created_at));

        if let Some(page_size) = params.page_size {
            results.truncate(page_size as usize);
        }

        Ok(results)
    }

    async fn delete_task(&self, task_id: &str) -> Result<(), A2aError> {
        let mut tasks = self.tasks.write().await;
        tasks
            .remove(task_id)
            .ok_or_else(|| A2aError::TaskNotFound { task_id: task_id.to_string() })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2a_protocol_types::{
        Artifact, ArtifactId, Message, MessageId, MessageRole, Part, TaskState, TaskStatus,
    };

    fn make_entry(id: &str, context_id: &str, state: TaskState) -> TaskStoreEntry {
        let now = Utc::now();
        TaskStoreEntry {
            id: id.to_string(),
            context_id: context_id.to_string(),
            status: TaskStatus::new(state),
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata: HashMap::new(),
            push_configs: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn make_message(text: &str) -> Message {
        Message {
            id: MessageId::new("msg-1"),
            role: MessageRole::User,
            parts: vec![Part::text(text)],
            task_id: None,
            context_id: None,
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        }
    }

    fn make_artifact(id: &str, text: &str) -> Artifact {
        Artifact::new(ArtifactId::new(id), vec![Part::text(text)])
    }

    #[tokio::test]
    async fn test_create_and_get_task() {
        let store = InMemoryTaskStore::new();
        let entry = make_entry("task-1", "ctx-1", TaskState::Submitted);

        store.create_task(entry).await.unwrap();
        let retrieved = store.get_task("task-1").await.unwrap();

        assert_eq!(retrieved.id, "task-1");
        assert_eq!(retrieved.context_id, "ctx-1");
        assert_eq!(retrieved.status.state, TaskState::Submitted);
    }

    #[tokio::test]
    async fn test_get_task_not_found() {
        let store = InMemoryTaskStore::new();
        let err = store.get_task("nonexistent").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"), "error should contain task ID: {msg}");
    }

    #[tokio::test]
    async fn test_update_status() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Submitted)).await.unwrap();

        let before = store.get_task("task-1").await.unwrap();
        let before_updated = before.updated_at;

        // Small delay to ensure timestamp differs
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        store.update_status("task-1", TaskStatus::new(TaskState::Working)).await.unwrap();

        let after = store.get_task("task-1").await.unwrap();
        assert_eq!(after.status.state, TaskState::Working);
        assert!(after.updated_at >= before_updated);
    }

    #[tokio::test]
    async fn test_update_status_not_found() {
        let store = InMemoryTaskStore::new();
        let err = store
            .update_status("nonexistent", TaskStatus::new(TaskState::Working))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_add_artifact() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();

        store.add_artifact("task-1", make_artifact("art-1", "hello")).await.unwrap();

        let task = store.get_task("task-1").await.unwrap();
        assert_eq!(task.artifacts.len(), 1);
        assert_eq!(task.artifacts[0].id, ArtifactId::new("art-1"));
    }

    #[tokio::test]
    async fn test_add_artifact_not_found() {
        let store = InMemoryTaskStore::new();
        let err =
            store.add_artifact("nonexistent", make_artifact("art-1", "hello")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_add_history_message() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();

        store.add_history_message("task-1", make_message("hello")).await.unwrap();

        let task = store.get_task("task-1").await.unwrap();
        assert_eq!(task.history.len(), 1);
    }

    #[tokio::test]
    async fn test_add_history_message_not_found() {
        let store = InMemoryTaskStore::new();
        let err =
            store.add_history_message("nonexistent", make_message("hello")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_delete_task() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Submitted)).await.unwrap();

        store.delete_task("task-1").await.unwrap();

        let err = store.get_task("task-1").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("task-1"));
    }

    #[tokio::test]
    async fn test_delete_task_not_found() {
        let store = InMemoryTaskStore::new();
        let err = store.delete_task("nonexistent").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_list_tasks_all() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Submitted)).await.unwrap();
        store.create_task(make_entry("task-2", "ctx-1", TaskState::Working)).await.unwrap();
        store.create_task(make_entry("task-3", "ctx-2", TaskState::Completed)).await.unwrap();

        let results = store.list_tasks(ListTasksParams::default()).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_list_tasks_filter_by_context_id() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Submitted)).await.unwrap();
        store.create_task(make_entry("task-2", "ctx-1", TaskState::Working)).await.unwrap();
        store.create_task(make_entry("task-3", "ctx-2", TaskState::Completed)).await.unwrap();

        let results = store
            .list_tasks(ListTasksParams {
                context_id: Some("ctx-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|t| t.context_id == "ctx-1"));
    }

    #[tokio::test]
    async fn test_list_tasks_filter_by_state() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Submitted)).await.unwrap();
        store.create_task(make_entry("task-2", "ctx-1", TaskState::Working)).await.unwrap();
        store.create_task(make_entry("task-3", "ctx-2", TaskState::Working)).await.unwrap();

        let results = store
            .list_tasks(ListTasksParams { state: Some(TaskState::Working), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|t| t.status.state == TaskState::Working));
    }

    #[tokio::test]
    async fn test_list_tasks_page_size() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Submitted)).await.unwrap();
        store.create_task(make_entry("task-2", "ctx-1", TaskState::Working)).await.unwrap();
        store.create_task(make_entry("task-3", "ctx-2", TaskState::Completed)).await.unwrap();

        let results = store
            .list_tasks(ListTasksParams { page_size: Some(2), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_list_tasks_combined_filters() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();
        store.create_task(make_entry("task-2", "ctx-1", TaskState::Submitted)).await.unwrap();
        store.create_task(make_entry("task-3", "ctx-2", TaskState::Working)).await.unwrap();
        store.create_task(make_entry("task-4", "ctx-1", TaskState::Working)).await.unwrap();

        let results = store
            .list_tasks(ListTasksParams {
                context_id: Some("ctx-1".to_string()),
                state: Some(TaskState::Working),
                page_size: Some(1),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].context_id, "ctx-1");
        assert_eq!(results[0].status.state, TaskState::Working);
    }

    #[tokio::test]
    async fn test_list_tasks_empty_store() {
        let store = InMemoryTaskStore::new();
        let results = store.list_tasks(ListTasksParams::default()).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_create_overwrites_existing() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Submitted)).await.unwrap();
        store.create_task(make_entry("task-1", "ctx-2", TaskState::Working)).await.unwrap();

        let task = store.get_task("task-1").await.unwrap();
        assert_eq!(task.context_id, "ctx-2");
        assert_eq!(task.status.state, TaskState::Working);
    }

    #[tokio::test]
    async fn test_multiple_artifacts() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();

        store.add_artifact("task-1", make_artifact("art-1", "first")).await.unwrap();
        store.add_artifact("task-1", make_artifact("art-2", "second")).await.unwrap();

        let task = store.get_task("task-1").await.unwrap();
        assert_eq!(task.artifacts.len(), 2);
    }

    #[tokio::test]
    async fn test_multiple_history_messages() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();

        store.add_history_message("task-1", make_message("first")).await.unwrap();
        store.add_history_message("task-1", make_message("second")).await.unwrap();

        let task = store.get_task("task-1").await.unwrap();
        assert_eq!(task.history.len(), 2);
    }

    #[tokio::test]
    async fn test_find_task_by_context_no_match() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();
        let result = store.find_task_by_context("ctx-other").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_find_task_by_context_excludes_terminal() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Completed)).await.unwrap();
        store.create_task(make_entry("task-2", "ctx-1", TaskState::Failed)).await.unwrap();
        store.create_task(make_entry("task-3", "ctx-1", TaskState::Canceled)).await.unwrap();
        store.create_task(make_entry("task-4", "ctx-1", TaskState::Rejected)).await.unwrap();
        let result = store.find_task_by_context("ctx-1").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_find_task_by_context_returns_most_recent_non_terminal() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();
        // Small delay to ensure different updated_at
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        store.create_task(make_entry("task-2", "ctx-1", TaskState::InputRequired)).await.unwrap();
        let result = store.find_task_by_context("ctx-1").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "task-2");
    }

    #[tokio::test]
    async fn test_find_task_by_context_isolates_contexts() {
        let store = InMemoryTaskStore::new();
        store.create_task(make_entry("task-1", "ctx-1", TaskState::Working)).await.unwrap();
        store.create_task(make_entry("task-2", "ctx-2", TaskState::Working)).await.unwrap();
        let result = store.find_task_by_context("ctx-1").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "task-1");
    }
}
