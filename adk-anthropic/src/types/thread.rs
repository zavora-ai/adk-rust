//! Message threads (beta `message-threads-2026-08-12`): the API keeps a conversation for a short
//! time, so a later request sends only the messages since the previous response.

use serde::{Deserialize, Serialize};

/// The beta header every request of a thread sends.
pub const MESSAGE_THREADS_BETA: &str = "message-threads-2026-08-12";

/// Starts a thread, or continues one from the previous response.
///
/// A create request carries the whole conversation, `system` and `tools`; a continue request
/// carries only the messages since `previous_message_id` and neither `system` nor `tools`.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::ThreadParam;
///
/// assert_eq!(serde_json::to_value(ThreadParam::Create).unwrap(), serde_json::json!({"type": "create"}));
/// assert_eq!(
///     serde_json::to_value(ThreadParam::continue_from("msg_01")).unwrap(),
///     serde_json::json!({"type": "continue", "previous_message_id": "msg_01"})
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ThreadParam {
    /// Stores the conversation, with the reply, under the response's `id`.
    Create,
    /// Continues the thread from the latest response.
    Continue {
        /// The `id` of the latest response of the thread.
        previous_message_id: String,
    },
}

impl ThreadParam {
    /// Continues the thread from the response `previous_message_id`.
    pub fn continue_from(previous_message_id: impl Into<String>) -> Self {
        Self::Continue { previous_message_id: previous_message_id.into() }
    }
}
