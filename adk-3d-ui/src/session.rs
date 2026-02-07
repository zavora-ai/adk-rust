use std::{
    collections::HashMap,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::Serialize;
use tokio::sync::{Mutex, RwLock, broadcast};
use uuid::Uuid;

use crate::policy::{ProposedAction, RiskTier};
use crate::protocol::{Seq, SessionId, SseEnvelope, SsePayload, UiEventRequest};

const CHANNEL_CAPACITY: usize = 256;
const MAX_STORED_EVENTS: usize = 128;

#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub event: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionDecision {
    Proposed,
    Approved,
    Rejected,
    Executed,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionAuditEntry {
    pub action_id: String,
    pub label: String,
    pub risk: RiskTier,
    pub decision: ActionDecision,
    pub ts: String,
}

#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    pub last_prompt: Option<String>,
    pub last_command: Option<String>,
    pub selected_id: Option<String>,
    pub last_intent_domain: Option<String>,
    pub last_node_ids: Vec<String>,
    pub pending_action: Option<ProposedAction>,
    pub audit_log: Vec<ActionAuditEntry>,
}

#[derive(Debug)]
struct SessionState {
    tx: broadcast::Sender<OutboundMessage>,
    server_seq: AtomicU64,
    events: Mutex<Vec<UiEventRequest>>,
    context: RwLock<SessionContext>,
}

impl SessionState {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            tx,
            server_seq: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
            context: RwLock::new(SessionContext::default()),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<SessionId, Arc<SessionState>>>>,
}

impl SessionManager {
    pub async fn create_session(&self) -> SessionId {
        let session_id = Uuid::new_v4().to_string();
        let state = Arc::new(SessionState::new());
        self.sessions.write().await.insert(session_id.clone(), state);
        session_id
    }

    pub async fn ensure_session(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        if !sessions.contains_key(session_id) {
            sessions.insert(session_id.to_string(), Arc::new(SessionState::new()));
        }
    }

    pub async fn subscribe(&self, session_id: &str) -> Option<broadcast::Receiver<OutboundMessage>> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|state| state.tx.subscribe())
    }

    pub async fn publish(&self, session_id: &str, payload: SsePayload) -> Option<Seq> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        let seq = state.server_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let envelope = SseEnvelope::new(seq, session_id.to_string(), payload.clone());
        let data = match serde_json::to_string(&envelope) {
            Ok(data) => data,
            Err(_) => return None,
        };
        let event = payload.event_name().to_string();
        let _ = state.tx.send(OutboundMessage { event, data });
        Some(seq)
    }

    pub async fn record_event(&self, session_id: &str, event: UiEventRequest) -> Option<()> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        let mut events = state.events.lock().await;
        events.push(event);
        if events.len() > MAX_STORED_EVENTS {
            let drain_to = events.len() - MAX_STORED_EVENTS;
            events.drain(0..drain_to);
        }
        Some(())
    }

    pub async fn last_server_seq(&self, session_id: &str) -> Option<Seq> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        Some(state.server_seq.load(Ordering::Relaxed))
    }

    pub async fn has_session(&self, session_id: &str) -> bool {
        let sessions = self.sessions.read().await;
        sessions.contains_key(session_id)
    }

    pub async fn get_context(&self, session_id: &str) -> Option<SessionContext> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        Some(state.context.read().await.clone())
    }

    pub async fn set_last_prompt(&self, session_id: &str, prompt: String) -> Option<()> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        state.context.write().await.last_prompt = Some(prompt);
        Some(())
    }

    pub async fn set_last_command(&self, session_id: &str, command: String) -> Option<()> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        state.context.write().await.last_command = Some(command);
        Some(())
    }

    pub async fn set_selected_id(&self, session_id: &str, selected_id: Option<String>) -> Option<()> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        state.context.write().await.selected_id = selected_id;
        Some(())
    }

    pub async fn update_plan_state(
        &self,
        session_id: &str,
        intent_domain: String,
        node_ids: Vec<String>,
    ) -> Option<()> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        let mut ctx = state.context.write().await;
        ctx.last_intent_domain = Some(intent_domain);
        ctx.last_node_ids = node_ids;
        Some(())
    }

    pub async fn set_pending_action(
        &self,
        session_id: &str,
        pending_action: Option<ProposedAction>,
    ) -> Option<()> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        state.context.write().await.pending_action = pending_action;
        Some(())
    }

    pub async fn append_audit_entry(
        &self,
        session_id: &str,
        entry: ActionAuditEntry,
    ) -> Option<()> {
        let sessions = self.sessions.read().await;
        let state = sessions.get(session_id)?;
        let mut ctx = state.context.write().await;
        ctx.audit_log.push(entry);
        if ctx.audit_log.len() > 128 {
            let drain_to = ctx.audit_log.len() - 128;
            ctx.audit_log.drain(0..drain_to);
        }
        Some(())
    }
}

pub fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::{ActionAuditEntry, ActionDecision, SessionManager};
    use crate::policy::{ProposedAction, RiskTier};

    #[tokio::test]
    async fn pending_action_roundtrip() {
        let sessions = SessionManager::default();
        let session_id = sessions.create_session().await;

        let action = ProposedAction {
            action_id: "action-xyz".to_string(),
            label: "Rollback".to_string(),
            rationale: "failure spike".to_string(),
            risk: RiskTier::Dangerous,
            requires_approval: true,
        };
        let _ = sessions
            .set_pending_action(&session_id, Some(action.clone()))
            .await;

        let context = sessions
            .get_context(&session_id)
            .await
            .expect("context exists");
        assert_eq!(
            context.pending_action.as_ref().map(|a| a.action_id.as_str()),
            Some("action-xyz")
        );
    }

    #[tokio::test]
    async fn audit_log_appends_entries() {
        let sessions = SessionManager::default();
        let session_id = sessions.create_session().await;

        let entry = ActionAuditEntry {
            action_id: "action-xyz".to_string(),
            label: "Rollback".to_string(),
            risk: RiskTier::Dangerous,
            decision: ActionDecision::Proposed,
            ts: chrono::Utc::now().to_rfc3339(),
        };
        let _ = sessions.append_audit_entry(&session_id, entry).await;

        let context = sessions
            .get_context(&session_id)
            .await
            .expect("context exists");
        assert_eq!(context.audit_log.len(), 1);
    }
}
