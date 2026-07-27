use std::path::PathBuf;

use adk_core::{AdkError, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use notify::{Config, Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use super::event_source::{EventSource, TriggerEvent};

/// Emits trigger events when files matching a glob pattern change.
///
/// Uses the `notify` crate for cross-platform filesystem watching.
///
/// # Example
///
/// ```rust,ignore
/// use adk_agent::ambient::FileWatchTrigger;
///
/// let trigger = FileWatchTrigger::new("/path/to/watch", "*.json")?;
/// ```
pub struct FileWatchTrigger {
    path: PathBuf,
    pattern: String,
    name: String,
}

impl FileWatchTrigger {
    /// Create a file watch trigger for the given path and glob pattern.
    ///
    /// Returns an error if the path does not exist.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Agent` if the watch path does not exist on the filesystem.
    pub fn new(path: impl Into<PathBuf>, pattern: &str) -> Result<Self> {
        let path = path.into();

        if !path.exists() {
            return Err(AdkError::agent(format!("watch path not found: {}", path.display())));
        }

        Ok(Self {
            name: format!("file_watch:{}", path.display()),
            path,
            pattern: pattern.to_string(),
        })
    }

    /// Check if an event path matches the configured glob pattern.
    fn matches_pattern(pattern: &str, event_path: &std::path::Path) -> bool {
        let file_name = event_path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Simple glob matching: support * and ? patterns
        Self::glob_match(pattern, file_name)
    }

    /// Simple glob pattern matching supporting `*` (any chars) and `?` (single char).
    fn glob_match(pattern: &str, text: &str) -> bool {
        let pattern_chars: Vec<char> = pattern.chars().collect();
        let text_chars: Vec<char> = text.chars().collect();

        Self::glob_match_recursive(&pattern_chars, &text_chars, 0, 0)
    }

    fn glob_match_recursive(pattern: &[char], text: &[char], pi: usize, ti: usize) -> bool {
        if pi == pattern.len() && ti == text.len() {
            return true;
        }
        if pi == pattern.len() {
            return false;
        }

        match pattern[pi] {
            '*' => {
                // * matches zero or more characters
                for i in ti..=text.len() {
                    if Self::glob_match_recursive(pattern, text, pi + 1, i) {
                        return true;
                    }
                }
                false
            }
            '?' => {
                // ? matches exactly one character
                if ti < text.len() {
                    Self::glob_match_recursive(pattern, text, pi + 1, ti + 1)
                } else {
                    false
                }
            }
            c => {
                if ti < text.len() && text[ti] == c {
                    Self::glob_match_recursive(pattern, text, pi + 1, ti + 1)
                } else {
                    false
                }
            }
        }
    }
}

#[async_trait]
impl EventSource for FileWatchTrigger {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, TriggerEvent>> {
        let (tx, mut rx) = mpsc::channel::<TriggerEvent>(256);
        let source_name = self.name.clone();
        let pattern = self.pattern.clone();
        let watch_path = self.path.clone();

        // Create a synchronous channel for notify → async bridge
        let (sync_tx, mut sync_rx) = mpsc::channel::<NotifyEvent>(256);

        // Spawn watcher in a blocking task since notify uses synchronous callbacks
        std::thread::spawn(move || {
            let rt = sync_tx;
            let mut watcher = match RecommendedWatcher::new(
                move |res: std::result::Result<NotifyEvent, notify::Error>| {
                    if let Ok(event) = res {
                        let _ = rt.blocking_send(event);
                    }
                },
                Config::default(),
            ) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!("file watch trigger failed to create watcher: {e}");
                    return;
                }
            };

            if let Err(e) = watcher.watch(&watch_path, RecursiveMode::Recursive) {
                tracing::warn!("file watch trigger failed to watch path: {e}");
                return;
            }

            // Keep the watcher alive until the thread is dropped
            // The watcher will be dropped when the thread is stopped
            std::thread::park();
        });

        // Spawn a task that filters events and forwards matching ones
        tokio::spawn(async move {
            while let Some(event) = sync_rx.recv().await {
                let event_kind = format!("{:?}", event.kind);

                for event_path in &event.paths {
                    if FileWatchTrigger::matches_pattern(&pattern, event_path) {
                        let trigger_event = TriggerEvent {
                            source: source_name.clone(),
                            payload: serde_json::json!({
                                "event": event_kind,
                                "path": event_path.display().to_string(),
                            }),
                            // A filesystem change has no caller.
                            principal: None,
                        };

                        if tx.send(trigger_event).await.is_err() {
                            tracing::debug!("file watch subscriber dropped");
                            return;
                        }
                    }
                }
            }
        });

        let stream = async_stream::stream! {
            while let Some(event) = rx.recv().await {
                yield event;
            }
        };

        Ok(Box::pin(stream))
    }
}

impl std::fmt::Debug for FileWatchTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWatchTrigger")
            .field("path", &self.path)
            .field("pattern", &self.pattern)
            .finish()
    }
}
