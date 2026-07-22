//! Permission handling for external ACP agent tool calls.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use agent_client_protocol::schema::v1::{
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    SelectedPermissionOutcome, ToolKind,
};

/// The complete security-relevant portion of a permission request from an ACP
/// agent. Custom policies receive the actual tool operation rather than a
/// display label from one of the response options.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// ACP session requesting the operation.
    pub session_id: String,
    /// Stable identifier for the tool call.
    pub tool_call_id: String,
    /// Human-readable title supplied for the tool call.
    pub title: String,
    /// ACP tool category used by clients for policy and presentation.
    pub kind: ToolKind,
    /// Raw tool arguments, when the agent supplied them.
    pub raw_input: Option<serde_json::Value>,
    /// Choices offered by the agent.
    pub options: Vec<PermissionOption>,
}

impl PermissionRequest {
    pub(crate) fn from_acp(request: &RequestPermissionRequest) -> Self {
        Self {
            session_id: request.session_id.to_string(),
            tool_call_id: request.tool_call.tool_call_id.to_string(),
            title: request
                .tool_call
                .fields
                .title
                .clone()
                .unwrap_or_else(|| request.tool_call.tool_call_id.to_string()),
            kind: request.tool_call.fields.kind.unwrap_or_default(),
            raw_input: request.tool_call.fields.raw_input.clone(),
            options: request
                .options
                .iter()
                .map(|option| PermissionOption {
                    id: option.option_id.to_string(),
                    name: option.name.clone(),
                    kind: option.kind,
                })
                .collect(),
        }
    }
}

/// A permission option exactly as offered by the ACP agent.
#[derive(Debug, Clone)]
pub struct PermissionOption {
    /// Opaque option ID that must be returned to the agent.
    pub id: String,
    /// Human-readable choice label.
    pub name: String,
    /// Whether this choice allows or rejects once or persistently.
    pub kind: PermissionOptionKind,
}

/// Future returned by an asynchronous permission policy.
pub type PermissionFuture = Pin<Box<dyn Future<Output = PermissionDecision> + Send>>;

/// Decision produced by an ADK-Rust permission policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Select a specific opaque option ID offered by the agent.
    Select(String),
    /// Prefer the offered one-time allow option.
    AllowOnce,
    /// Prefer the offered persistent allow option.
    AllowAlways,
    /// Prefer an offered one-time rejection, then a persistent rejection.
    Deny,
}

impl PermissionDecision {
    /// Allow this operation once using the actual option ID supplied by the agent.
    pub fn allow_once() -> Self {
        Self::AllowOnce
    }

    /// Deny the operation using an offered rejection option when available.
    pub fn deny() -> Self {
        Self::Deny
    }
}

impl fmt::Display for PermissionDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Select(id) => write!(f, "select({id})"),
            Self::AllowOnce => write!(f, "allow_once"),
            Self::AllowAlways => write!(f, "allow_always"),
            Self::Deny => write!(f, "deny"),
        }
    }
}

/// Policy for permission requests made by a spawned ACP agent.
#[derive(Default)]
pub enum PermissionPolicy {
    /// Select a one-time allow option, falling back to allow-always only when
    /// the agent did not offer a one-time choice.
    AutoApprove,
    /// Reject every requested operation. This is the safe default.
    #[default]
    DenyAll,
    /// Apply application-specific policy to the complete request.
    Custom(Box<dyn Fn(&PermissionRequest) -> PermissionDecision + Send + Sync>),
    /// Wait for an asynchronous decision, such as a desktop approval dialog,
    /// web UI, or remote policy service.
    AsyncCustom(Box<dyn Fn(PermissionRequest) -> PermissionFuture + Send + Sync>),
}

impl PermissionPolicy {
    /// Build a policy that can await a human or external policy decision.
    pub fn async_custom<F, Fut>(handler: F) -> Self
    where
        F: Fn(PermissionRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = PermissionDecision> + Send + 'static,
    {
        Self::AsyncCustom(Box::new(move |request| Box::pin(handler(request))))
    }

    /// Evaluate this policy for a request.
    pub async fn decide(&self, request: &PermissionRequest) -> PermissionDecision {
        match self {
            Self::AutoApprove => PermissionDecision::AllowOnce,
            Self::DenyAll => PermissionDecision::Deny,
            Self::Custom(handler) => handler(request),
            Self::AsyncCustom(handler) => handler(request.clone()).await,
        }
    }
}

impl fmt::Debug for PermissionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AutoApprove => write!(f, "PermissionPolicy::AutoApprove"),
            Self::DenyAll => write!(f, "PermissionPolicy::DenyAll"),
            Self::Custom(_) => write!(f, "PermissionPolicy::Custom(...)"),
            Self::AsyncCustom(_) => write!(f, "PermissionPolicy::AsyncCustom(...)"),
        }
    }
}

pub(crate) fn outcome_for(
    request: &PermissionRequest,
    decision: &PermissionDecision,
) -> RequestPermissionOutcome {
    let exact = |id: &str| request.options.iter().find(|option| option.id == id);
    let by_kind = |kind| request.options.iter().find(|option| option.kind == kind);

    let selected = match decision {
        PermissionDecision::Select(id) => exact(id),
        PermissionDecision::AllowOnce => by_kind(PermissionOptionKind::AllowOnce)
            .or_else(|| by_kind(PermissionOptionKind::AllowAlways)),
        PermissionDecision::AllowAlways => by_kind(PermissionOptionKind::AllowAlways)
            .or_else(|| by_kind(PermissionOptionKind::AllowOnce)),
        PermissionDecision::Deny => by_kind(PermissionOptionKind::RejectOnce)
            .or_else(|| by_kind(PermissionOptionKind::RejectAlways)),
    };

    selected.map_or(RequestPermissionOutcome::Cancelled, |option| {
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option.id.clone()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(options: &[(PermissionOptionKind, &str)]) -> PermissionRequest {
        PermissionRequest {
            session_id: "session-1".into(),
            tool_call_id: "tool-1".into(),
            title: "Delete generated file".into(),
            kind: ToolKind::Delete,
            raw_input: Some(serde_json::json!({"path":"generated.rs"})),
            options: options
                .iter()
                .map(|(kind, id)| PermissionOption {
                    id: (*id).into(),
                    name: (*id).into(),
                    kind: *kind,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn auto_approve_ignores_reject_option_order() {
        let request = request(&[
            (PermissionOptionKind::RejectOnce, "no"),
            (PermissionOptionKind::AllowOnce, "yes-once"),
        ]);
        let outcome = outcome_for(&request, &PermissionPolicy::AutoApprove.decide(&request).await);
        assert!(
            matches!(outcome, RequestPermissionOutcome::Selected(selected) if selected.option_id.to_string() == "yes-once")
        );
    }

    #[tokio::test]
    async fn async_policy_can_wait_for_a_human_decision() {
        let request = request(&[
            (PermissionOptionKind::RejectOnce, "no"),
            (PermissionOptionKind::AllowOnce, "yes-once"),
        ]);
        let policy = PermissionPolicy::async_custom(|request| async move {
            assert_eq!(request.tool_call_id, "tool-1");
            PermissionDecision::AllowOnce
        });
        let outcome = outcome_for(&request, &policy.decide(&request).await);
        assert!(
            matches!(outcome, RequestPermissionOutcome::Selected(selected) if selected.option_id.to_string() == "yes-once")
        );
    }

    #[test]
    fn deny_selects_an_offered_rejection_id() {
        let request = request(&[
            (PermissionOptionKind::AllowOnce, "allow"),
            (PermissionOptionKind::RejectAlways, "never"),
        ]);
        let outcome = outcome_for(&request, &PermissionDecision::Deny);
        assert!(
            matches!(outcome, RequestPermissionOutcome::Selected(selected) if selected.option_id.to_string() == "never")
        );
    }

    #[test]
    fn fabricated_id_is_cancelled() {
        let request = request(&[(PermissionOptionKind::AllowOnce, "real-id")]);
        assert!(matches!(
            outcome_for(&request, &PermissionDecision::Select("made-up".into())),
            RequestPermissionOutcome::Cancelled
        ));
    }
}
