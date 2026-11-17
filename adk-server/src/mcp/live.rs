//! What the server knows of runs in flight in this process: the latest progress line of each,
//! and a channel that tells waiting calls when a run moved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{Event, Part, RunRecord};
use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::background::{ParkReason, WorkerHooks};

/// Most characters of model text one progress line quotes.
const TEXT_SNIPPET_CHARS: usize = 160;

/// Updates kept for a subscriber that falls behind; older ones are dropped.
const CHANNEL_CAPACITY: usize = 64;

/// A change in a run in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Update {
    /// The run did something worth reporting.
    Progress(String),
    /// The run parked or finished; read it from the store.
    Settled,
}

struct Channel {
    tx: broadcast::Sender<Update>,
    status: Option<String>,
}

impl Channel {
    fn new() -> Self {
        Self { tx: broadcast::channel(CHANNEL_CAPACITY).0, status: None }
    }
}

/// Runs in flight, by run id.
#[derive(Default)]
pub(super) struct Live {
    runs: Mutex<HashMap<String, Channel>>,
}

impl Live {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Channel>> {
        self.runs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Updates of `run_id` from now on.
    pub(super) fn subscribe(&self, run_id: &str) -> broadcast::Receiver<Update> {
        self.lock().entry(run_id.to_string()).or_insert_with(Channel::new).tx.subscribe()
    }

    /// The latest progress line of `run_id`, while it is in flight here.
    pub(super) fn status(&self, run_id: &str) -> Option<String> {
        self.lock().get(run_id).and_then(|channel| channel.status.clone())
    }

    /// Drops `run_id` when nothing listens to it.
    pub(super) fn release(&self, run_id: &str) {
        let mut runs = self.lock();
        if runs.get(run_id).is_some_and(|channel| channel.tx.receiver_count() == 0) {
            runs.remove(run_id);
        }
    }

    fn progress(&self, run_id: &str, line: String) {
        let mut runs = self.lock();
        let channel = runs.entry(run_id.to_string()).or_insert_with(Channel::new);
        channel.status = Some(line.clone());
        // No receiver is not an error: the line is kept as the status.
        let _ = channel.tx.send(Update::Progress(line));
    }

    fn settled(&self, run_id: &str, finished: bool) {
        let mut runs = self.lock();
        if let Some(channel) = runs.get_mut(run_id) {
            channel.status = None;
            let _ = channel.tx.send(Update::Settled);
            if finished && channel.tx.receiver_count() == 0 {
                runs.remove(run_id);
            }
        }
    }
}

/// The [`WorkerHooks`] that feed [`Live`] from a worker running the host's agents.
pub(super) struct LiveHooks {
    pub(super) live: Arc<Live>,
    pub(super) app_name: String,
    pub(super) targets: Vec<String>,
}

impl LiveHooks {
    fn serves(&self, run: &RunRecord) -> bool {
        run.app_name == self.app_name && self.targets.contains(&run.target)
    }
}

#[async_trait]
impl WorkerHooks for LiveHooks {
    async fn on_claim(&self, run: &RunRecord) {
        if self.serves(run) {
            self.live.progress(&run.run_id, "running".to_string());
        }
    }

    async fn on_park(&self, run: &RunRecord, _reason: ParkReason) {
        if self.serves(run) {
            self.live.settled(&run.run_id, false);
        }
    }

    async fn on_finish(&self, run: &RunRecord) {
        if self.serves(run) {
            self.live.settled(&run.run_id, run.state.is_terminal());
        }
    }

    async fn on_event(&self, run: &RunRecord, event: &Event) {
        if self.serves(run)
            && let Some(line) = progress_line(event)
        {
            self.live.progress(&run.run_id, line);
        }
    }
}

/// The progress line an event reports, if any: a tool call, a tool result, a call held for
/// approval, or the start of the model's text. Partial chunks report nothing.
pub(super) fn progress_line(event: &Event) -> Option<String> {
    if let Some(request) = &event.actions.tool_confirmation {
        return Some(format!("waiting for approval to run {}", request.tool_name));
    }
    if event.llm_response.partial {
        return None;
    }
    let parts = &event.llm_response.content.as_ref()?.parts;
    let mut lines = Vec::new();
    for part in parts {
        match part {
            Part::FunctionCall { name, .. } => lines.push(format!("calling {name}")),
            Part::FunctionResponse { function_response, .. } => {
                lines.push(format!("{} returned", function_response.name));
            }
            Part::Text { text } if !text.trim().is_empty() => {
                let snippet: String = text.trim().chars().take(TEXT_SNIPPET_CHARS).collect();
                let cut = if text.trim().chars().count() > TEXT_SNIPPET_CHARS { "…" } else { "" };
                lines.push(format!("{}: {snippet}{cut}", event.author));
            }
            _ => {}
        }
    }
    (!lines.is_empty()).then(|| lines.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, FunctionResponseData};
    use serde_json::json;

    fn event(parts: Vec<Part>, partial: bool) -> Event {
        let mut event = Event::new("inv-1");
        event.author = "assistant".to_string();
        event.llm_response.content = Some(Content { role: "model".to_string(), parts });
        event.llm_response.partial = partial;
        event
    }

    #[test]
    fn tool_calls_and_results_are_reported() {
        let call = Part::FunctionCall {
            name: "send_email".to_string(),
            args: json!({}),
            id: Some("c1".to_string()),
            thought_signature: None,
        };
        assert_eq!(progress_line(&event(vec![call], false)).as_deref(), Some("calling send_email"));
        let response = Part::FunctionResponse {
            function_response: FunctionResponseData::new("send_email", json!({ "ok": true })),
            id: Some("c1".to_string()),
            annotations: None,
        };
        assert_eq!(
            progress_line(&event(vec![response], false)).as_deref(),
            Some("send_email returned")
        );
    }

    #[test]
    fn partial_chunks_report_nothing_and_text_is_cut() {
        let text = Part::Text { text: "x".repeat(400) };
        assert_eq!(progress_line(&event(vec![text.clone()], true)), None);
        let line = progress_line(&event(vec![text], false)).unwrap();
        assert!(line.starts_with("assistant: xxx"));
        assert!(line.ends_with('…'));
        assert_eq!(line.chars().count(), "assistant: ".len() + TEXT_SNIPPET_CHARS + 1);
    }

    #[tokio::test]
    async fn a_subscriber_hears_progress_then_the_settle() {
        let live = Live::default();
        let mut updates = live.subscribe("run-1");
        live.progress("run-1", "calling quote".to_string());
        assert_eq!(live.status("run-1").as_deref(), Some("calling quote"));
        live.settled("run-1", true);
        assert_eq!(updates.recv().await.unwrap(), Update::Progress("calling quote".to_string()));
        assert_eq!(updates.recv().await.unwrap(), Update::Settled);
        assert_eq!(live.status("run-1"), None);
        drop(updates);
        live.release("run-1");
        assert!(live.lock().is_empty());
    }
}
