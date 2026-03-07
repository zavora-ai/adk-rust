//! Shared state utility functions for session backends.
//!
//! These functions implement the three-tier state model used by all session
//! backends (SQLite, PostgreSQL, Redis). Keys are partitioned by prefix:
//!
//! - `app:` → app-level state (prefix stripped)
//! - `user:` → user-level state (prefix stripped)
//! - `temp:` → ephemeral, dropped on persistence
//! - everything else → session-level state

use crate::session::{KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER};
use serde_json::Value;
use std::collections::HashMap;

/// Split a flat state map into (app, user, session) tiers.
///
/// Keys with `app:` prefix go to the app tier (prefix stripped).
/// Keys with `user:` prefix go to the user tier (prefix stripped).
/// Keys with `temp:` prefix are dropped.
/// All other keys go to the session tier unchanged.
pub fn extract_state_deltas(
    delta: &HashMap<String, Value>,
) -> (HashMap<String, Value>, HashMap<String, Value>, HashMap<String, Value>) {
    let mut app = HashMap::new();
    let mut user = HashMap::new();
    let mut session = HashMap::new();

    for (key, value) in delta {
        if let Some(clean) = key.strip_prefix(KEY_PREFIX_APP) {
            app.insert(clean.to_string(), value.clone());
        } else if let Some(clean) = key.strip_prefix(KEY_PREFIX_USER) {
            user.insert(clean.to_string(), value.clone());
        } else if !key.starts_with(KEY_PREFIX_TEMP) {
            session.insert(key.clone(), value.clone());
        }
    }

    (app, user, session)
}

/// Merge three state tiers back into a flat map with prefixes restored.
pub fn merge_states(
    app: &HashMap<String, Value>,
    user: &HashMap<String, Value>,
    session: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    let mut merged = session.clone();
    for (k, v) in app {
        merged.insert(format!("{KEY_PREFIX_APP}{k}"), v.clone());
    }
    for (k, v) in user {
        merged.insert(format!("{KEY_PREFIX_USER}{k}"), v.clone());
    }
    merged
}
