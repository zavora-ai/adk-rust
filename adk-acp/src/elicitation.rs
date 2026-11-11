//! Answering an ACP agent's structured questions (`elicitation/create`).
//!
//! An ACP agent may ask its client for structured input, either as a form
//! described by a restricted JSON schema or by sending the user to a URL. A
//! client advertises which modes it handles in `initialize`, and an agent asks
//! only in those modes.
//!
//! adk-acp advertises elicitation support only when an [`AcpElicitation`] is
//! configured with [`AcpAgentConfig::elicitation`](crate::AcpAgentConfig::elicitation).
//! Without one the agent never asks.

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAction, ElicitationId,
};
use async_trait::async_trait;

/// Answers the structured questions an ACP agent asks its client.
///
/// # Example
///
/// ```rust
/// use adk_acp::AcpElicitation;
/// use adk_acp::agent_client_protocol::Error;
/// use adk_acp::agent_client_protocol::v1::schema::{
///     CreateElicitationRequest, CreateElicitationResponse, ElicitationAcceptAction,
///     ElicitationAction, ElicitationContentValue, ElicitationMode,
/// };
/// use async_trait::async_trait;
/// use std::collections::BTreeMap;
///
/// /// Answers every form with the configured branch name.
/// #[derive(Debug)]
/// struct BranchName(String);
///
/// #[async_trait]
/// impl AcpElicitation for BranchName {
///     async fn elicit(
///         &self,
///         request: CreateElicitationRequest,
///     ) -> Result<CreateElicitationResponse, Error> {
///         let ElicitationMode::Form(form) = &request.mode else {
///             return Ok(CreateElicitationResponse::new(ElicitationAction::Decline));
///         };
///         let mut content = BTreeMap::new();
///         for name in form.requested_schema.properties.keys() {
///             content.insert(name.clone(), ElicitationContentValue::from(self.0.clone()));
///         }
///         Ok(CreateElicitationResponse::new(ElicitationAction::Accept(
///             ElicitationAcceptAction::new().content(content),
///         )))
///     }
/// }
/// ```
#[async_trait]
pub trait AcpElicitation: std::fmt::Debug + Send + Sync {
    /// Whether form elicitations are handled. Advertised as `elicitation.form`.
    fn supports_form(&self) -> bool {
        true
    }

    /// Whether URL elicitations are handled. Advertised as `elicitation.url`.
    ///
    /// A URL elicitation asks the user to open a page; the agent later reports
    /// it finished with `elicitation/complete`, which reaches
    /// [`completed`](Self::completed).
    fn supports_url(&self) -> bool {
        false
    }

    /// Answer one elicitation: accept with content, decline, or cancel.
    ///
    /// When the agent withdraws the request before this returns, adk-acp
    /// answers `cancel` on the handler's behalf.
    async fn elicit(
        &self,
        request: CreateElicitationRequest,
    ) -> Result<CreateElicitationResponse, Error>;

    /// The agent reported that the URL elicitation `elicitation_id` completed.
    async fn completed(&self, _elicitation_id: &ElicitationId) {}
}

/// Declines every elicitation while still advertising form support, so an
/// agent that requires the capability can start and degrade gracefully.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeclineElicitation;

#[async_trait]
impl AcpElicitation for DeclineElicitation {
    async fn elicit(
        &self,
        _request: CreateElicitationRequest,
    ) -> Result<CreateElicitationResponse, Error> {
        Ok(CreateElicitationResponse::new(ElicitationAction::Decline))
    }
}
