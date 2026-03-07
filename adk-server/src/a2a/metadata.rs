use adk_core::types::{SessionId, UserId};
use serde_json::Value;
use std::collections::HashMap;

pub const META_PREFIX: &str = "adk_";

#[derive(Clone)]
pub struct InvocationMeta {
    pub user_id: UserId,
    pub session_id: SessionId,
    pub event_meta: HashMap<String, Value>,
}

// Intentionally omit Debug to avoid cleartext logging of user_id/session_id
impl std::fmt::Debug for InvocationMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvocationMeta")
            .field("user_id", &"[REDACTED]")
            .field("session_id", &self.session_id)
            .field("event_meta_keys", &self.event_meta.keys().collect::<Vec<_>>())
            .finish()
    }
}

pub fn to_a2a_meta_key(key: &str) -> String {
    format!("{}{}", META_PREFIX, key)
}

pub fn to_invocation_meta(
    app_name: &str,
    context_id: &str,
    user_id: Option<&str>,
) -> InvocationMeta {
    let user_id = UserId::new(
        user_id.map(|s| s.to_string()).unwrap_or_else(|| format!("A2A_USER_{}", context_id)),
    )
    .unwrap();
    let session_id = SessionId::new(context_id.to_string()).unwrap();

    let mut event_meta = HashMap::new();
    event_meta.insert(to_a2a_meta_key("app_name"), Value::String(app_name.to_string()));
    event_meta.insert(to_a2a_meta_key("user_id"), Value::String(user_id.to_string()));
    event_meta.insert(to_a2a_meta_key("session_id"), Value::String(session_id.to_string()));

    InvocationMeta { user_id, session_id, event_meta }
}

pub fn to_event_meta(meta: &InvocationMeta, event: &adk_core::Event) -> HashMap<String, Value> {
    let mut result = meta.event_meta.clone();

    result.insert(to_a2a_meta_key("invocation_id"), Value::String(event.invocation_id.to_string()));
    result.insert(to_a2a_meta_key("author"), Value::String(event.author.clone()));
    if !event.branch.is_empty() {
        result.insert(to_a2a_meta_key("branch"), Value::String(event.branch.clone()));
    }

    result
}

pub fn set_actions_meta(
    mut meta: HashMap<String, Value>,
    actions: &adk_core::EventActions,
) -> HashMap<String, Value> {
    if actions.escalate {
        meta.insert(to_a2a_meta_key("escalate"), Value::Bool(true));
    }
    if let Some(agent) = &actions.transfer_to_agent {
        meta.insert(to_a2a_meta_key("transfer_to_agent"), Value::String(agent.clone()));
    }
    meta
}
