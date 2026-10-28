//! What a decision on the approvals route may be (spec 3.3 §7.2, §11 S-I11, S-I13): the
//! kernel's [`DecideGuard`] over `POST /api/approvals/{id}/decide`.
//!
//! The route is answered under §7.2 for the surface the server established, never one a
//! client declares:
//!
//! | Caller | Row | Allow once, Edit | High risk | Add an agent |
//! |---|---|---|---|---|
//! | The kernel's socket or pipe | paired local terminal | Yes | Yes | Yes |
//! | A paired phone | phone | Yes | No (confirm it's you in the home) | Yes, at low and medium risk |
//! | A paired remote terminal | terminal | Yes | No (on the kernel's own terminal) | Yes, at low and medium risk |
//! | A paired tray | tray | Allow once only | No (opens the home) | No (opens the home) |
//! | An operator credential over HTTP | `api` | Yes | No (confirm it's you in the home) | Yes, at low and medium risk |
//!
//! Deny and a written answer stay open to every caller. A refusal answers `403
//! surface.decide.not_here`. A decision admitted from the socket, the pipe or an operator
//! credential is recorded as `approvals.decide` with its surface, how and transport before it
//! reaches the inbox, and a failed record refuses it (a device's is recorded as it is
//! admitted). A suggested agent approved or refused here is added or ended at once, its record
//! naming this surface rather than the inbox.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalAction, ApprovalRequest, ApprovalRisk, ApprovalState, ErrorCategory,
    ErrorComponent, RequestContext,
};
use adk_server::approvals::{DecideCall, DecideGuard};
use async_trait::async_trait;
use axum::http::Extensions;
use serde_json::json;

use super::ControlTransport;
use super::routes::ControlBackend;
use super::server::PeerProcess;
use crate::agents::{AGENT_CREATE_TOOL_NAME, AgentCaller, Presence};
use crate::error::DECIDE_NOT_HERE_CODE;
use crate::pairing::{DEVICE_ID_METADATA, DEVICE_KIND_METADATA};

/// The row of §7.2 the server established for a caller of the approvals route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    PairedLocal,
    Phone,
    RemoteTerminal,
    Tray,
    Api,
}

impl Row {
    /// A device by the kind its pairing code was issued with; otherwise the kernel's socket or
    /// pipe, or an operator credential over HTTP.
    fn of(caller: &RequestContext, extensions: &Extensions) -> Self {
        if caller.metadata.contains_key(DEVICE_ID_METADATA) {
            return match caller.metadata.get(DEVICE_KIND_METADATA).map(String::as_str) {
                Some("phone") => Self::Phone,
                Some("tray") => Self::Tray,
                _ => Self::RemoteTerminal,
            };
        }
        match transport(extensions) {
            ControlTransport::Socket | ControlTransport::Pipe => Self::PairedLocal,
            ControlTransport::Http => Self::Api,
        }
    }

    fn surface(self) -> &'static str {
        match self {
            Self::PairedLocal | Self::RemoteTerminal => "terminal",
            Self::Phone => "phone",
            Self::Tray => "tray",
            Self::Api => "api",
        }
    }

    fn how(self) -> &'static str {
        match self {
            Self::PairedLocal | Self::RemoteTerminal => "keyboard",
            Self::Phone => "touch",
            Self::Tray => "pointer",
            Self::Api => "api",
        }
    }

    fn is_device(self) -> bool {
        matches!(self, Self::Phone | Self::RemoteTerminal | Self::Tray)
    }

    /// Why this row may not give `action` to `request`, rated high risk or not; `None` when
    /// it may. Deny and a written answer are never refused.
    fn refusal(
        self,
        request: &ApprovalRequest,
        action: ApprovalAction,
        high: bool,
    ) -> Option<&'static str> {
        if !matches!(action, ApprovalAction::Approve | ApprovalAction::Edit) {
            return None;
        }
        let adds_agent = request.tool_name == AGENT_CREATE_TOOL_NAME;
        match self {
            Self::PairedLocal => None,
            Self::Tray if action == ApprovalAction::Edit => {
                Some("the tray does not change a call: edit it in the home")
            }
            Self::Tray if adds_agent => Some(
                "an agent is added in the home, on the phone or in the terminal, never from the \
                 tray",
            ),
            Self::Phone | Self::RemoteTerminal | Self::Tray | Self::Api if high => Some(
                "this decision is high risk: confirm it's you and approve it in the home, or on \
                 the kernel's own terminal",
            ),
            Self::Phone | Self::RemoteTerminal | Self::Tray | Self::Api => None,
        }
    }
}

/// The transport the kernel's control plane marked the request with; HTTP when none did, as
/// on a server's own app.
fn transport(extensions: &Extensions) -> ControlTransport {
    extensions.get::<ControlTransport>().copied().unwrap_or(ControlTransport::Http)
}

/// The kernel's check of every decision on the approvals route (spec 3.3 §7.2, S-I11, S-I13).
pub(super) struct KernelDecideGuard {
    backend: Arc<dyn ControlBackend>,
}

impl KernelDecideGuard {
    pub(super) fn new(backend: Arc<dyn ControlBackend>) -> Self {
        Self { backend }
    }

    /// The agent table's caller for `call`: who, from which surface and how, through which
    /// device and transport, with what presence and process (spec 3.3 S-I18).
    fn agent_caller(call: &DecideCall<'_>, row: Row) -> AgentCaller {
        let mut caller = AgentCaller::new(
            SubjectRef::human(call.caller.user_id.clone()),
            row.surface(),
            row.how(),
        )
        .with_transport(transport(call.extensions));
        if let Some(device) = call.caller.metadata.get(DEVICE_ID_METADATA) {
            caller = caller.with_device(device.clone());
        }
        if row == Row::PairedLocal {
            caller = caller.with_presence(Presence::PairedLocal);
        }
        if let Some(PeerProcess(pid)) = call.extensions.get::<PeerProcess>() {
            caller = caller.with_peer_pid(*pid);
        }
        caller
    }
}

#[async_trait]
impl DecideGuard for KernelDecideGuard {
    async fn check(&self, call: &DecideCall<'_>) -> Result<(), AdkError> {
        let request = call.request;
        if request.state != ApprovalState::Pending {
            return Ok(());
        }
        let row = Row::of(call.caller, call.extensions);
        let high = request.risk >= ApprovalRisk::High
            || super::pairing::rated_high(self.backend.kernel(), request);
        if let Some(reason) = row.refusal(request, call.action, high) {
            return Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Forbidden,
                DECIDE_NOT_HERE_CODE,
                reason,
            ));
        }
        if row.is_device() {
            return Ok(());
        }
        let extra = [
            ("surface", json!(row.surface())),
            ("how", json!(row.how())),
            ("answer", json!(call.action)),
        ];
        super::pairing::record_as(
            self.backend.as_ref(),
            SubjectRef::human(call.caller.user_id.clone()),
            transport(call.extensions),
            "approvals.decide",
            Some(&request.id),
            &extra,
        )
        .await
        .map_err(AdkError::from)
    }

    async fn decided(&self, call: &DecideCall<'_>, decided: &ApprovalRequest) {
        if decided.tool_name != AGENT_CREATE_TOOL_NAME || decided.state == ApprovalState::Pending {
            return;
        }
        let Some(kernel) = self.backend.kernel() else { return };
        let caller = Self::agent_caller(call, Row::of(call.caller, call.extensions));
        kernel.agents().answered(&decided.id, &caller).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(kind: Option<&str>) -> RequestContext {
        let mut context = RequestContext {
            user_id: "alice".into(),
            scopes: vec![],
            metadata: std::collections::HashMap::new(),
        };
        if let Some(kind) = kind {
            context.metadata.insert(DEVICE_ID_METADATA.into(), "dev_1".into());
            context.metadata.insert(DEVICE_KIND_METADATA.into(), kind.into());
        }
        context
    }

    fn request(tool: &str) -> ApprovalRequest {
        let call = adk_core::ToolConfirmationRequest::new(tool, json!({}));
        let scope = adk_core::ApprovalScope::new("adk-rust", "alice", "s-1");
        let policy = adk_core::ApprovalPolicy::new([adk_core::ApproverSelector::human("alice")]);
        ApprovalRequest::new("r-1", scope, &call, policy)
    }

    /// The rows follow §7.2: the server's transport and the device's kind pick the row, and
    /// a row allows what its column says.
    #[test]
    fn each_row_decides_what_its_column_allows() {
        let mut socket = Extensions::new();
        socket.insert(ControlTransport::Socket);
        let http = Extensions::new();
        assert_eq!(Row::of(&context(None), &socket), Row::PairedLocal);
        assert_eq!(Row::of(&context(None), &http), Row::Api);
        assert_eq!(Row::of(&context(Some("phone")), &http), Row::Phone);
        assert_eq!(Row::of(&context(Some("tray")), &http), Row::Tray);
        assert_eq!(Row::of(&context(Some("terminal")), &http), Row::RemoteTerminal);

        let agent = request(AGENT_CREATE_TOOL_NAME);
        let call = request("send_email");
        let (approve, edit, reject) =
            (ApprovalAction::Approve, ApprovalAction::Edit, ApprovalAction::Reject);
        for row in [Row::PairedLocal, Row::Phone, Row::RemoteTerminal, Row::Api] {
            assert_eq!(row.refusal(&agent, approve, false), None, "{row:?}");
            assert_eq!(row.refusal(&call, edit, false), None, "{row:?}");
        }
        for row in [Row::Phone, Row::RemoteTerminal, Row::Tray, Row::Api] {
            assert!(row.refusal(&call, approve, true).is_some(), "{row:?}");
            assert!(row.refusal(&agent, approve, true).is_some(), "{row:?}");
            assert_eq!(row.refusal(&call, reject, true), None, "{row:?}");
        }
        assert_eq!(Row::PairedLocal.refusal(&agent, approve, true), None);
        assert!(Row::Tray.refusal(&agent, approve, false).is_some());
        assert!(Row::Tray.refusal(&call, edit, false).is_some());
        assert_eq!(Row::Tray.refusal(&call, approve, false), None);
        assert_eq!(Row::Tray.refusal(&agent, reject, false), None);
    }
}
