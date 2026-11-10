//! A2A v1.0.0 executor — wraps task lifecycle with TaskStore persistence
//! and state machine validation.
//!
//! The [`V1Executor`] creates tasks in the [`TaskStore`], validates state
//! transitions via [`can_transition_to`], and persists status updates and
//! artifacts throughout the task lifecycle.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use a2a_protocol_types::TaskPushNotificationConfig;
use a2a_protocol_types::artifact::Artifact;
use a2a_protocol_types::events::{StreamResponse, TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};
use chrono::Utc;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use super::convert::ADK_EXTENSION_URI;
use super::error::A2aError;
use super::processor::ArtifactUpdate;
use super::push::PushNotificationSender;
use super::state_machine::can_transition_to;
use super::task_store::{OWNER_METADATA_KEY, TaskStore, TaskStoreEntry};

/// Task lifecycle management for A2A v1.0.0.
///
/// Wraps a [`TaskStore`] and validates every state transition against the A2A state
/// machine. Every change to a task is made under that task's lock, so concurrent requests
/// and a running agent never overwrite each other's updates. Each status and artifact
/// update is sent, in order and without loss, to the task's
/// [`subscribe`](Self::subscribe)rs, and to its push notification configs when a push
/// sender is set.
pub struct V1Executor {
    task_store: Arc<dyn TaskStore>,
    /// Live update channels by task ID, kept only for tasks that have subscribers.
    subscribers: Mutex<HashMap<String, Vec<mpsc::UnboundedSender<StreamResponse>>>>,
    /// One lock per task, held while the task is read and written back.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Delivers updates to push notification configs.
    push_sender: Mutex<Option<Arc<dyn PushNotificationSender>>>,
    /// Tasks with at least one push notification config.
    push_tasks: Mutex<HashSet<String>>,
    /// Each pushed task's delivery queue, drained in order by its own worker.
    push_queues: Mutex<HashMap<String, mpsc::UnboundedSender<StreamResponse>>>,
}

impl V1Executor {
    /// Creates a new executor backed by the given task store.
    pub fn new(task_store: Arc<dyn TaskStore>) -> Self {
        Self {
            task_store,
            subscribers: Mutex::default(),
            locks: Mutex::default(),
            push_sender: Mutex::default(),
            push_tasks: Mutex::default(),
            push_queues: Mutex::default(),
        }
    }

    /// Delivers every status and artifact update of a task with push notification configs
    /// through `sender`, in order. A failed delivery is logged, not fatal.
    pub fn set_push_sender(&self, sender: Arc<dyn PushNotificationSender>) {
        *lock(&self.push_sender) = Some(sender);
    }

    /// Receives every update to `task_id` from now on, in order, until the task ends (the
    /// channel closes after a terminal status). The receiver never misses an update.
    pub fn subscribe(&self, task_id: &str) -> mpsc::UnboundedReceiver<StreamResponse> {
        let (sender, receiver) = mpsc::unbounded_channel();
        lock(&self.subscribers).entry(task_id.to_string()).or_default().push(sender);
        receiver
    }

    /// Sends `update` to the subscribers and push notification configs of `task_id`.
    pub fn publish(&self, task_id: &str, update: StreamResponse) {
        let terminal = matches!(
            &update,
            StreamResponse::StatusUpdate(event) if is_terminal(event.status.state)
        );
        {
            let mut subscribers = lock(&self.subscribers);
            if let Some(senders) = subscribers.get_mut(task_id) {
                senders.retain(|sender| sender.send(update.clone()).is_ok());
                if terminal || senders.is_empty() {
                    subscribers.remove(task_id);
                }
            }
        }
        self.dispatch_push(task_id, update, terminal);
    }

    fn dispatch_push(&self, task_id: &str, update: StreamResponse, terminal: bool) {
        if !lock(&self.push_tasks).contains(task_id) {
            return;
        }
        let Some(sender) = lock(&self.push_sender).clone() else { return };
        let mut queues = lock(&self.push_queues);
        let queue = queues.entry(task_id.to_string()).or_insert_with(|| {
            spawn_push_worker(self.task_store.clone(), sender, task_id.to_string())
        });
        if queue.send(update).is_err() {
            tracing::warn!(task_id, "push delivery queue closed; update not delivered");
        }
        if terminal {
            // The worker delivers what is queued, then stops.
            queues.remove(task_id);
            lock(&self.push_tasks).remove(task_id);
        }
    }

    /// Saves `entry` over the version it was read at, one version later.
    async fn save_versioned(
        &self,
        mut entry: TaskStoreEntry,
        version: u64,
    ) -> Result<(), A2aError> {
        entry.version = version + 1;
        self.task_store.save_task_if_version(entry, version).await
    }

    /// Holds `task_id`'s lock until the guard drops.
    async fn lock_task(&self, task_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let task_lock = lock(&self.locks).entry(task_id.to_string()).or_default().clone();
        task_lock.lock_owned().await
    }

    /// Creates a new task in `Submitted` state.
    ///
    /// # Errors
    ///
    /// Returns an error if the task store fails to persist the task.
    pub async fn create_task(
        &self,
        task_id: &str,
        context_id: &str,
    ) -> Result<TaskStoreEntry, A2aError> {
        self.create_task_for(task_id, context_id, None).await
    }

    /// Creates a new task in `Submitted` state, owned by `owner` when one is given.
    ///
    /// # Errors
    ///
    /// Returns an error if the task store fails to persist the task.
    pub async fn create_task_for(
        &self,
        task_id: &str,
        context_id: &str,
        owner: Option<&str>,
    ) -> Result<TaskStoreEntry, A2aError> {
        self.create_task_with_metadata(task_id, context_id, owner, HashMap::new()).await
    }

    /// Creates a new task in `Submitted` state with `metadata`, owned by `owner` when one
    /// is given.
    ///
    /// # Errors
    ///
    /// Returns an error if the task store fails to persist the task.
    pub async fn create_task_with_metadata(
        &self,
        task_id: &str,
        context_id: &str,
        owner: Option<&str>,
        mut metadata: HashMap<String, serde_json::Value>,
    ) -> Result<TaskStoreEntry, A2aError> {
        let now = Utc::now();
        if let Some(owner) = owner {
            metadata.insert(
                OWNER_METADATA_KEY.to_string(),
                serde_json::Value::String(owner.to_string()),
            );
        }
        let entry = TaskStoreEntry {
            id: task_id.to_string(),
            context_id: context_id.to_string(),
            status: TaskStatus::with_timestamp(TaskState::Submitted),
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata,
            push_configs: Vec::new(),
            created_at: now,
            updated_at: now,
            version: 0,
        };
        self.task_store.create_task(entry.clone()).await?;
        Ok(entry)
    }

    /// Transitions a task to a new state, with `message` as its status text.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::InvalidStateTransition`] if the transition is not allowed,
    /// [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn transition_state(
        &self,
        task_id: &str,
        context_id: &str,
        new_state: TaskState,
        message: Option<String>,
    ) -> Result<TaskStatusUpdateEvent, A2aError> {
        let parts = message.map(a2a_protocol_types::Part::text).into_iter().collect();
        self.transition_state_with_parts(task_id, context_id, new_state, parts).await
    }

    /// Transitions a task to a new state, with `parts` (if any) as its status message.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::InvalidStateTransition`] if the transition is not allowed,
    /// [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn transition_state_with_parts(
        &self,
        task_id: &str,
        context_id: &str,
        new_state: TaskState,
        parts: Vec<a2a_protocol_types::Part>,
    ) -> Result<TaskStatusUpdateEvent, A2aError> {
        self.update_status(task_id, context_id, new_state, parts, None).await
    }

    /// Transitions a task to `new_state` with `parts` (if any) as its status message, and
    /// sends the update with `metadata`, or the task's own metadata when `None`.
    ///
    /// The previous status message moves into the task's history, as A2A task managers
    /// do. A task an ADK agent runs marks the update with the ADK extension.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::InvalidStateTransition`] if the transition is not allowed,
    /// [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn update_status(
        &self,
        task_id: &str,
        context_id: &str,
        new_state: TaskState,
        parts: Vec<a2a_protocol_types::Part>,
        metadata: Option<Map<String, Value>>,
    ) -> Result<TaskStatusUpdateEvent, A2aError> {
        let _guard = self.lock_task(task_id).await;
        let mut entry = self.task_store.get_task(task_id).await?;
        let version = entry.version;
        can_transition_to(entry.status.state, new_state)?;

        let mut status = TaskStatus::with_timestamp(new_state);
        if !parts.is_empty() {
            status.message = Some(a2a_protocol_types::Message {
                id: a2a_protocol_types::MessageId::new(uuid::Uuid::new_v4().to_string()),
                role: a2a_protocol_types::MessageRole::Agent,
                parts,
                task_id: Some(TaskId::new(task_id)),
                context_id: Some(ContextId::new(context_id)),
                reference_task_ids: None,
                extensions: None,
                metadata: None,
            });
        }
        if let Some(previous) = entry.status.message.take() {
            entry.history.push(previous);
        }
        entry.status = status.clone();
        entry.updated_at = Utc::now();
        let adk_task = entry.metadata.contains_key(ADK_EXTENSION_URI);
        let mut metadata = metadata.unwrap_or_else(|| task_metadata(&entry));
        self.save_versioned(entry, version).await?;
        if adk_task {
            metadata.insert(ADK_EXTENSION_URI.to_string(), Value::Bool(true));
        }

        let event = TaskStatusUpdateEvent {
            task_id: TaskId::new(task_id),
            context_id: ContextId::new(context_id),
            status,
            metadata: (!metadata.is_empty()).then_some(Value::Object(metadata)),
        };
        self.publish(task_id, StreamResponse::StatusUpdate(event.clone()));
        Ok(event)
    }

    /// Records an artifact for a task and sends it to the task's subscribers.
    ///
    /// Persists the artifact to the task store, sends it to the task's subscribers, and
    /// returns the corresponding [`TaskArtifactUpdateEvent`].
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn record_artifact(
        &self,
        task_id: &str,
        context_id: &str,
        artifact: Artifact,
    ) -> Result<TaskArtifactUpdateEvent, A2aError> {
        let event = self.persist_artifact(task_id, context_id, artifact).await?;
        self.publish(task_id, StreamResponse::ArtifactUpdate(event.clone()));
        Ok(event)
    }

    /// Records an artifact for a task without sending it to the task's subscribers.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn persist_artifact(
        &self,
        task_id: &str,
        context_id: &str,
        artifact: Artifact,
    ) -> Result<TaskArtifactUpdateEvent, A2aError> {
        let _guard = self.lock_task(task_id).await;
        let mut entry = self.task_store.get_task(task_id).await?;
        let version = entry.version;
        merge_artifact(&mut entry.artifacts, artifact.clone(), false);
        entry.updated_at = Utc::now();
        let metadata = task_metadata(&entry);
        self.save_versioned(entry, version).await?;

        let event = TaskArtifactUpdateEvent {
            task_id: TaskId::new(task_id),
            context_id: ContextId::new(context_id),
            artifact,
            append: None,
            last_chunk: None,
            metadata: (!metadata.is_empty()).then_some(Value::Object(metadata)),
        };
        Ok(event)
    }

    /// Applies one of a run's artifact updates to the task, as A2A task managers do: the
    /// same artifact ID with `append` adds its parts and metadata, without `append`
    /// replaces the artifact. Sends the update to the task's subscribers.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub(super) async fn apply_artifact_update(
        &self,
        task_id: &str,
        context_id: &str,
        update: ArtifactUpdate,
    ) -> Result<TaskArtifactUpdateEvent, A2aError> {
        {
            let _guard = self.lock_task(task_id).await;
            let mut entry = self.task_store.get_task(task_id).await?;
            let version = entry.version;
            merge_artifact(&mut entry.artifacts, update.artifact.clone(), update.append);
            entry.updated_at = Utc::now();
            self.save_versioned(entry, version).await?;
        }
        let event = TaskArtifactUpdateEvent {
            task_id: TaskId::new(task_id),
            context_id: ContextId::new(context_id),
            artifact: update.artifact,
            append: Some(update.append),
            last_chunk: Some(update.last_chunk),
            metadata: update.metadata.filter(|meta| !meta.is_empty()).map(Value::Object),
        };
        self.publish(task_id, StreamResponse::ArtifactUpdate(event.clone()));
        Ok(event)
    }

    /// Saves `config` on the task, replacing a config with the same ID.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn save_push_config(
        &self,
        task_id: &str,
        config: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2aError> {
        let _guard = self.lock_task(task_id).await;
        let mut entry = self.task_store.get_task(task_id).await?;
        let version = entry.version;
        match entry.push_configs.iter_mut().find(|existing| existing.id == config.id) {
            Some(existing) => *existing = config.clone(),
            None => entry.push_configs.push(config.clone()),
        }
        entry.updated_at = Utc::now();
        self.save_versioned(entry, version).await?;
        lock(&self.push_tasks).insert(task_id.to_string());
        Ok(config)
    }

    /// Removes the push config `config_id` from the task. Removing a config the task does
    /// not have succeeds, as A2A config stores do.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn delete_push_config(&self, task_id: &str, config_id: &str) -> Result<(), A2aError> {
        let _guard = self.lock_task(task_id).await;
        let mut entry = self.task_store.get_task(task_id).await?;
        let version = entry.version;
        entry.push_configs.retain(|config| config.id.as_deref() != Some(config_id));
        let empty = entry.push_configs.is_empty();
        entry.updated_at = Utc::now();
        self.save_versioned(entry, version).await?;
        if empty {
            lock(&self.push_tasks).remove(task_id);
        }
        Ok(())
    }

    /// Appends `message` to the task's history.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn add_history_message(
        &self,
        task_id: &str,
        message: a2a_protocol_types::Message,
    ) -> Result<(), A2aError> {
        let _guard = self.lock_task(task_id).await;
        let mut entry = self.task_store.get_task(task_id).await?;
        let version = entry.version;
        entry.history.push(message);
        entry.updated_at = Utc::now();
        self.save_versioned(entry, version).await
    }

    /// Takes a task that waits on its caller for a run: moves it from `INPUT_REQUIRED` or
    /// `AUTH_REQUIRED` back to `SUBMITTED` and records `message`, all under the task's
    /// lock, so a second message that races this one finds the task taken.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::UnsupportedOperation`] when the task does not wait on its caller,
    /// [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn claim_waiting_task(
        &self,
        task_id: &str,
        message: a2a_protocol_types::Message,
    ) -> Result<TaskStoreEntry, A2aError> {
        let _guard = self.lock_task(task_id).await;
        let mut entry = self.task_store.get_task(task_id).await?;
        let version = entry.version;
        if !matches!(entry.status.state, TaskState::InputRequired | TaskState::AuthRequired) {
            return Err(A2aError::UnsupportedOperation {
                method: format!("a message to task {task_id} while it is {:?}", entry.status.state),
            });
        }
        if let Some(previous) = entry.status.message.take() {
            entry.history.push(previous);
        }
        entry.history.push(message);
        entry.status = TaskStatus::with_timestamp(TaskState::Submitted);
        entry.updated_at = Utc::now();
        entry.version = version + 1;
        self.task_store.save_task_if_version(entry.clone(), version).await?;
        Ok(entry)
    }

    /// Asks a waiting task's caller again for the call `missing` that `message` left
    /// unanswered, as adk-go does: records `message`, keeps the task `INPUT_REQUIRED`, and
    /// sends a status whose message repeats the pending calls and adds
    /// `no input provided for function call ID "<missing>"`, marked `validation_error`.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::TaskNotFound`] if the task does not exist, or a store error.
    pub async fn ask_again(
        &self,
        task_id: &str,
        message: a2a_protocol_types::Message,
        missing: &str,
    ) -> Result<TaskStatusUpdateEvent, A2aError> {
        let _guard = self.lock_task(task_id).await;
        let mut entry = self.task_store.get_task(task_id).await?;
        let version = entry.version;
        let previous = entry.status.message.take();
        let mut parts: Vec<a2a_protocol_types::Part> = previous
            .as_ref()
            .map(|previous| {
                previous
                    .parts
                    .iter()
                    .filter(|part| {
                        part.metadata
                            .as_ref()
                            .and_then(|meta| meta.get(VALIDATION_ERROR_METADATA_KEY))
                            != Some(&Value::Bool(true))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut notice = a2a_protocol_types::Part::text(format!(
            "no input provided for function call ID {missing:?}"
        ));
        notice.metadata = Some(serde_json::json!({ VALIDATION_ERROR_METADATA_KEY: true }));
        parts.push(notice);
        if let Some(previous) = previous {
            entry.history.push(previous);
        }
        entry.history.push(message);
        let context_id = entry.context_id.clone();
        let mut status = TaskStatus::with_timestamp(TaskState::InputRequired);
        status.message = Some(a2a_protocol_types::Message {
            id: a2a_protocol_types::MessageId::new(uuid::Uuid::new_v4().to_string()),
            role: a2a_protocol_types::MessageRole::Agent,
            parts,
            task_id: Some(TaskId::new(task_id)),
            context_id: Some(ContextId::new(&context_id)),
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        });
        entry.status = status.clone();
        entry.updated_at = Utc::now();
        let adk_task = entry.metadata.contains_key(ADK_EXTENSION_URI);
        self.save_versioned(entry, version).await?;
        let event = TaskStatusUpdateEvent {
            task_id: TaskId::new(task_id),
            context_id: ContextId::new(&context_id),
            status,
            metadata: adk_task.then(|| serde_json::json!({ ADK_EXTENSION_URI: true })),
        };
        self.publish(task_id, StreamResponse::StatusUpdate(event.clone()));
        Ok(event)
    }

    /// Transitions a task to `Failed` with an error message.
    ///
    /// # Errors
    ///
    /// Returns [`A2aError::InvalidStateTransition`] if the task is already in a terminal
    /// state, or a store error.
    pub async fn fail_task(
        &self,
        task_id: &str,
        context_id: &str,
        error_message: &str,
    ) -> Result<TaskStatusUpdateEvent, A2aError> {
        self.transition_state_with_parts(
            task_id,
            context_id,
            TaskState::Failed,
            vec![super::convert::error_message_part(error_message)],
        )
        .await
    }

    /// Returns a reference to the underlying task store.
    pub fn task_store(&self) -> &Arc<dyn TaskStore> {
        &self.task_store
    }
}

/// Part metadata `validation_error`: the text says which pending call an answer missed.
pub const VALIDATION_ERROR_METADATA_KEY: &str = "validation_error";

/// The task's metadata as its updates carry it: without the owner.
fn task_metadata(entry: &TaskStoreEntry) -> Map<String, Value> {
    entry
        .metadata
        .iter()
        .filter(|(key, _)| *key != OWNER_METADATA_KEY)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Adds `artifact` to `artifacts`: with `append`, its parts and metadata go onto the
/// artifact with the same ID; otherwise it replaces that artifact. An artifact with a new
/// ID is added at the end.
fn merge_artifact(artifacts: &mut Vec<Artifact>, artifact: Artifact, append: bool) {
    let Some(existing) = artifacts.iter_mut().find(|existing| existing.id == artifact.id) else {
        artifacts.push(artifact);
        return;
    };
    if !append {
        *existing = artifact;
        return;
    }
    existing.parts.extend(artifact.parts);
    if let Some(Value::Object(added)) = artifact.metadata {
        match &mut existing.metadata {
            Some(Value::Object(metadata)) => metadata.extend(added),
            _ => existing.metadata = Some(Value::Object(added)),
        }
    }
}

/// Starts a worker that delivers `task_id`'s queued updates to the task's push configs, in
/// order, and stops when the queue closes.
fn spawn_push_worker(
    store: Arc<dyn TaskStore>,
    sender: Arc<dyn PushNotificationSender>,
    task_id: String,
) -> mpsc::UnboundedSender<StreamResponse> {
    let (queue, mut updates) = mpsc::unbounded_channel::<StreamResponse>();
    tokio::spawn(async move {
        while let Some(update) = updates.recv().await {
            let configs = match store.get_task(&task_id).await {
                Ok(entry) => entry.push_configs,
                Err(_) => return,
            };
            for config in &configs {
                if let Err(error) = sender.send_update(&update, config).await {
                    tracing::warn!(%task_id, url = %config.url, %error, "push notification not delivered");
                }
            }
        }
    });
    queue
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_terminal(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected
    )
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
            version: 0,
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
    async fn artifact_updates_append_to_or_replace_the_artifact_with_their_id() {
        use super::super::processor::ArtifactUpdate;
        use a2a_protocol_types::artifact::ArtifactId;
        let executor = make_executor();
        executor.create_task("t-merge", "ctx").await.unwrap();
        let update = |id: &str, text: &str, append: bool| ArtifactUpdate {
            artifact: Artifact::new(
                ArtifactId::new(id),
                vec![a2a_protocol_types::Part::text(text)],
            ),
            append,
            last_chunk: false,
            metadata: None,
        };
        let mut updates = executor.subscribe("t-merge");
        for (id, text, append) in
            [("a", "one", false), ("a", "two", true), ("b", "temp", false), ("b", "new", false)]
        {
            executor
                .apply_artifact_update("t-merge", "ctx", update(id, text, append))
                .await
                .unwrap();
        }

        let entry = executor.task_store().get_task("t-merge").await.unwrap();
        let texts: Vec<Vec<&str>> = entry
            .artifacts
            .iter()
            .map(|artifact| artifact.parts.iter().filter_map(|part| part.text_content()).collect())
            .collect();
        assert_eq!(texts, vec![vec!["one", "two"], vec!["new"]]);
        let mut sent = 0;
        while let Ok(update) = updates.try_recv() {
            assert!(matches!(update, StreamResponse::ArtifactUpdate(_)));
            sent += 1;
        }
        assert_eq!(sent, 4, "a subscriber gets every update");
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
