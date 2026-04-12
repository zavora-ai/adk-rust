//! A2A v1.0.0 executor — wraps task lifecycle with TaskStore persistence
//! and state machine validation.
//!
//! The [`V1Executor`] creates tasks in the [`TaskStore`], validates state
//! transitions via [`can_transition_to`], and persists status updates and
//! artifacts throughout the task lifecycle.

use std::collections::HashMap;
use std::sync::Arc;

use a2a_protocol_types::artifact::Artifact;
use a2a_protocol_types::events::{TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};
use chrono::Utc;

use super::error::A2aError;
use super::state_machine::can_transition_to;
use super::task_store::{TaskStore, TaskStoreEntry};

/// V1 executor that wraps task lifecycle with [`TaskStore`] persistence
/// and state machine validation.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_server::a2a::v1::executor::V1Executor;
/// use adk_server::a2a::v1::task_store::InMemoryTaskStore;
///
/// let store = Arc::new(InMemoryTaskStore::new());
/// let executor = V1Executor::new(store);
///
/// let entry = executor.create_task("task-1", "ctx-1").await?;
/// let event = executor.transition_state("task-1", "ctx-1", TaskState::Working, None).await?;
/// ```
pub struct V1Executor {
    task_store: Arc<dyn TaskStore>,
}

impl V1Executor {
    /// Creates a new V1 executor backed by the given task store.
    pub fn new(task_store: Arc<dyn TaskStore>) -> Self {
        Self { task_store }
    }

    /// Creates a new task in the store with `TaskState::Submitted`.
    ///
    /// # Errors
    ///
    /// Returns an error if the task store fails to persist the entry.
    pub async fn create_task(
        &self,
        task_id: &str,
        context_id: &str,
    ) -> Result<TaskStoreEntry, A2aError> {
        let now = Utc::now();
        let entry = TaskStoreEntry {
            id: task_id.to_string(),
            context_id: context_id.to_string(),
            status: TaskStatus::with_timestamp(TaskState::Submitted),
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata: HashMap::new(),
            push_configs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        self.task_store.create_task(entry.clone()).await?;
        Ok(entry)
    }

    /// Transitions a task to a new state, validating via the state machine.
    ///
    /// Retrieves the current task from the store, validates the transition
    /// with [`can_transition_to`], persists the new status, and returns
    /// a [`TaskStatusUpdateEvent`] with `contextId`, `timestamp`, and `metadata`.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or
    /// [`A2aError::InvalidStateTransition`] if the transition is not allowed.
    pub async fn transition_state(
        &self,
        task_id: &str,
        context_id: &str,
        new_state: TaskState,
        message: Option<String>,
    ) -> Result<TaskStatusUpdateEvent, A2aError> {
        let current = self.task_store.get_task(task_id).await?;
        can_transition_to(current.status.state, new_state)?;

        let mut status = TaskStatus::with_timestamp(new_state);
        if let Some(msg_text) = message {
            status.message = Some(a2a_protocol_types::Message {
                id: a2a_protocol_types::MessageId::new(format!("status-msg-{task_id}")),
                role: a2a_protocol_types::MessageRole::Agent,
                parts: vec![a2a_protocol_types::Part::text(msg_text)],
                task_id: None,
                context_id: None,
                reference_task_ids: None,
                extensions: None,
                metadata: None,
            });
        }

        self.task_store.update_status(task_id, status.clone()).await?;

        let metadata = if current.metadata.is_empty() {
            None
        } else {
            let obj: serde_json::Map<String, serde_json::Value> =
                current.metadata.into_iter().collect();
            Some(serde_json::Value::Object(obj))
        };

        Ok(TaskStatusUpdateEvent {
            task_id: TaskId::new(task_id),
            context_id: ContextId::new(context_id),
            status,
            metadata,
        })
    }

    /// Records an artifact for a task.
    ///
    /// Persists the artifact to the task store and returns a
    /// [`TaskArtifactUpdateEvent`] with `contextId` and `metadata`.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist.
    pub async fn record_artifact(
        &self,
        task_id: &str,
        context_id: &str,
        artifact: Artifact,
    ) -> Result<TaskArtifactUpdateEvent, A2aError> {
        let current = self.task_store.get_task(task_id).await?;
        self.task_store.add_artifact(task_id, artifact.clone()).await?;

        let metadata = if current.metadata.is_empty() {
            None
        } else {
            let obj: serde_json::Map<String, serde_json::Value> =
                current.metadata.into_iter().collect();
            Some(serde_json::Value::Object(obj))
        };

        Ok(TaskArtifactUpdateEvent {
            task_id: TaskId::new(task_id),
            context_id: ContextId::new(context_id),
            artifact,
            append: None,
            last_chunk: None,
            metadata,
        })
    }

    /// Marks a task as failed with error details.
    ///
    /// Validates the transition to `TaskState::Failed` via the state machine,
    /// persists the failed status with an error message, and returns the
    /// corresponding [`TaskStatusUpdateEvent`].
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or
    /// [`A2aError::InvalidStateTransition`] if the transition is not allowed.
    pub async fn fail_task(
        &self,
        task_id: &str,
        context_id: &str,
        error_message: &str,
    ) -> Result<TaskStatusUpdateEvent, A2aError> {
        self.transition_state(
            task_id,
            context_id,
            TaskState::Failed,
            Some(error_message.to_string()),
        )
        .await
    }

    /// Returns a reference to the underlying task store.
    pub fn task_store(&self) -> &Arc<dyn TaskStore> {
        &self.task_store
    }
}

#[cfg(test)]
mod tests {
    use super::super::task_store::InMemoryTaskStore;
    use super::*;

    fn make_executor() -> V1Executor {
        V1Executor::new(Arc::new(InMemoryTaskStore::new()))
    }

    /// Asserts that a timestamp is `Some` and looks like a valid RFC 3339 string.
    fn assert_valid_timestamp(ts: &Option<String>) {
        let ts = ts.as_ref().expect("timestamp should be Some");
        assert!(ts.contains('T'), "timestamp should contain 'T': {ts}");
        assert!(ts.len() >= 19, "timestamp should be at least 19 chars: {ts}");
    }

    #[tokio::test]
    async fn create_task_persists_with_submitted_state() {
        let executor = make_executor();
        let entry = executor.create_task("t1", "ctx-1").await.unwrap();

        assert_eq!(entry.id, "t1");
        assert_eq!(entry.context_id, "ctx-1");
        assert_eq!(entry.status.state, TaskState::Submitted);
        assert_valid_timestamp(&entry.status.timestamp);

        // Verify persisted in store
        let stored = executor.task_store().get_task("t1").await.unwrap();
        assert_eq!(stored.id, "t1");
        assert_eq!(stored.status.state, TaskState::Submitted);
        assert_valid_timestamp(&stored.status.timestamp);
    }

    #[tokio::test]
    async fn transition_state_validates_and_persists() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-1").await.unwrap();

        let event =
            executor.transition_state("t1", "ctx-1", TaskState::Working, None).await.unwrap();

        assert_eq!(event.task_id, TaskId::new("t1"));
        assert_eq!(event.context_id, ContextId::new("ctx-1"));
        assert_eq!(event.status.state, TaskState::Working);
        assert_valid_timestamp(&event.status.timestamp);

        // Verify persisted
        let stored = executor.task_store().get_task("t1").await.unwrap();
        assert_eq!(stored.status.state, TaskState::Working);
        assert_valid_timestamp(&stored.status.timestamp);
    }

    #[tokio::test]
    async fn transition_state_with_message() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-1").await.unwrap();

        let event = executor
            .transition_state(
                "t1",
                "ctx-1",
                TaskState::Working,
                Some("processing started".to_string()),
            )
            .await
            .unwrap();

        assert!(event.status.message.is_some());
        let msg = event.status.message.unwrap();
        assert_eq!(msg.parts.len(), 1);
    }

    #[tokio::test]
    async fn transition_state_rejects_invalid_transition() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-1").await.unwrap();

        // SUBMITTED → COMPLETED is not allowed
        let err =
            executor.transition_state("t1", "ctx-1", TaskState::Completed, None).await.unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("SUBMITTED"));
        assert!(msg.contains("COMPLETED"));
    }

    #[tokio::test]
    async fn transition_state_task_not_found() {
        let executor = make_executor();

        let err = executor
            .transition_state("nonexistent", "ctx-1", TaskState::Working, None)
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
    }

    #[tokio::test]
    async fn record_artifact_persists_and_returns_event() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-1").await.unwrap();

        let artifact = Artifact::new(
            a2a_protocol_types::ArtifactId::new("art-1"),
            vec![a2a_protocol_types::Part::text("result")],
        );

        let event = executor.record_artifact("t1", "ctx-1", artifact).await.unwrap();

        assert_eq!(event.task_id, TaskId::new("t1"));
        assert_eq!(event.context_id, ContextId::new("ctx-1"));
        assert_eq!(event.artifact.id, a2a_protocol_types::ArtifactId::new("art-1"));

        // Verify persisted
        let stored = executor.task_store().get_task("t1").await.unwrap();
        assert_eq!(stored.artifacts.len(), 1);
    }

    #[tokio::test]
    async fn record_artifact_task_not_found() {
        let executor = make_executor();

        let artifact = Artifact::new(
            a2a_protocol_types::ArtifactId::new("art-1"),
            vec![a2a_protocol_types::Part::text("result")],
        );

        let err = executor.record_artifact("nonexistent", "ctx-1", artifact).await.unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
    }

    #[tokio::test]
    async fn fail_task_transitions_to_failed() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-1").await.unwrap();
        executor.transition_state("t1", "ctx-1", TaskState::Working, None).await.unwrap();

        let event = executor.fail_task("t1", "ctx-1", "something went wrong").await.unwrap();

        assert_eq!(event.status.state, TaskState::Failed);
        assert!(event.status.message.is_some());

        // Verify persisted
        let stored = executor.task_store().get_task("t1").await.unwrap();
        assert_eq!(stored.status.state, TaskState::Failed);
    }

    #[tokio::test]
    async fn fail_task_from_terminal_state_is_rejected() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-1").await.unwrap();
        executor.transition_state("t1", "ctx-1", TaskState::Working, None).await.unwrap();
        executor.transition_state("t1", "ctx-1", TaskState::Completed, None).await.unwrap();

        // COMPLETED → FAILED is not allowed
        let err = executor.fail_task("t1", "ctx-1", "late error").await.unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("COMPLETED"));
        assert!(msg.contains("FAILED"));
    }

    #[tokio::test]
    async fn full_lifecycle_submitted_working_completed() {
        let executor = make_executor();

        // Create task (SUBMITTED)
        let entry = executor.create_task("t1", "ctx-1").await.unwrap();
        assert_eq!(entry.status.state, TaskState::Submitted);
        assert_valid_timestamp(&entry.status.timestamp);

        // Transition to WORKING
        let event =
            executor.transition_state("t1", "ctx-1", TaskState::Working, None).await.unwrap();
        assert_eq!(event.status.state, TaskState::Working);
        assert_valid_timestamp(&event.status.timestamp);

        // Record artifact
        let artifact = Artifact::new(
            a2a_protocol_types::ArtifactId::new("art-1"),
            vec![a2a_protocol_types::Part::text("output")],
        );
        let art_event = executor.record_artifact("t1", "ctx-1", artifact).await.unwrap();
        assert_eq!(art_event.artifact.id, a2a_protocol_types::ArtifactId::new("art-1"));

        // Transition to COMPLETED
        let event =
            executor.transition_state("t1", "ctx-1", TaskState::Completed, None).await.unwrap();
        assert_eq!(event.status.state, TaskState::Completed);
        assert_valid_timestamp(&event.status.timestamp);

        // Verify final state in store
        let stored = executor.task_store().get_task("t1").await.unwrap();
        assert_eq!(stored.status.state, TaskState::Completed);
        assert_eq!(stored.artifacts.len(), 1);
        assert_valid_timestamp(&stored.status.timestamp);
    }

    #[tokio::test]
    async fn metadata_included_in_events() {
        let store = Arc::new(InMemoryTaskStore::new());
        let executor = V1Executor::new(store.clone());

        // Create task with metadata
        let now = Utc::now();
        let mut metadata = HashMap::new();
        metadata.insert("key".to_string(), serde_json::json!("value"));
        let entry = TaskStoreEntry {
            id: "t1".to_string(),
            context_id: "ctx-1".to_string(),
            status: TaskStatus::new(TaskState::Submitted),
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata,
            push_configs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        store.create_task(entry).await.unwrap();

        // Transition — metadata should be included in event
        let event =
            executor.transition_state("t1", "ctx-1", TaskState::Working, None).await.unwrap();

        assert!(event.metadata.is_some());
        let meta = event.metadata.unwrap();
        assert_eq!(meta["key"], "value");
    }

    #[tokio::test]
    async fn context_id_included_in_status_event() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-abc").await.unwrap();

        let event =
            executor.transition_state("t1", "ctx-abc", TaskState::Working, None).await.unwrap();

        assert_eq!(event.context_id, ContextId::new("ctx-abc"));
    }

    #[tokio::test]
    async fn context_id_included_in_artifact_event() {
        let executor = make_executor();
        executor.create_task("t1", "ctx-abc").await.unwrap();

        let artifact = Artifact::new(
            a2a_protocol_types::ArtifactId::new("art-1"),
            vec![a2a_protocol_types::Part::text("data")],
        );

        let event = executor.record_artifact("t1", "ctx-abc", artifact).await.unwrap();

        assert_eq!(event.context_id, ContextId::new("ctx-abc"));
    }

    #[tokio::test]
    async fn task_store_accessor() {
        let store = Arc::new(InMemoryTaskStore::new());
        let executor = V1Executor::new(store.clone());

        // Verify task_store() returns the same store
        executor.create_task("t1", "ctx-1").await.unwrap();
        let task = executor.task_store().get_task("t1").await.unwrap();
        assert_eq!(task.id, "t1");
    }
}
