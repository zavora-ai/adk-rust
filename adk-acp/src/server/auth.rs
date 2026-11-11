//! Authentication for the ACP server.
//!
//! ACP v1 lets an agent advertise `authMethods` in its `initialize` response
//! and answer `authenticate` and `logout`. With the unstable
//! `auth/status` surface it also reports its authentication state and
//! notifies the client when that state changes.
//!
//! The server advertises no methods and requires no authentication unless an
//! [`AcpAuthenticator`] is configured with
//! [`AcpServerConfigBuilder::authenticator`](super::AcpServerConfigBuilder::authenticator).
//! With one configured and [`AcpAuthenticator::required`] returning `true`,
//! every session request is answered with the `auth_required` error
//! (`-32000`) until the client authenticates on that connection.

use std::sync::atomic::{AtomicU8, Ordering};

use agent_client_protocol::v1::schema::{AuthAccount, AuthMethod, AuthState, AuthenticateRequest};
use async_trait::async_trait;

use super::error::AcpServerError;

/// Decides how ACP clients authenticate with an ADK-Rust agent.
///
/// # Example
///
/// ```rust
/// use adk_acp::agent_client_protocol::v1::schema::{AuthMethod, AuthMethodAgent, AuthenticateRequest};
/// use adk_acp::server::{AcpAuthenticator, AcpServerError};
/// use async_trait::async_trait;
///
/// /// Accepts the `workspace-token` method when the server holds a token.
/// struct WorkspaceToken(Option<String>);
///
/// #[async_trait]
/// impl AcpAuthenticator for WorkspaceToken {
///     fn methods(&self) -> Vec<AuthMethod> {
///         vec![AuthMethod::Agent(AuthMethodAgent::new("workspace-token", "Workspace token"))]
///     }
///
///     async fn authenticate(&self, request: &AuthenticateRequest) -> Result<(), AcpServerError> {
///         match (&*request.method_id.0, &self.0) {
///             ("workspace-token", Some(_)) => Ok(()),
///             _ => Err(AcpServerError::MalformedMessage("no workspace token is configured".into())),
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait AcpAuthenticator: Send + Sync {
    /// The methods advertised as `authMethods` in the `initialize` response.
    fn methods(&self) -> Vec<AuthMethod>;

    /// Whether session requests need a successful `authenticate` first.
    ///
    /// Defaults to `true`. Return `false` to advertise optional methods, for
    /// example to switch accounts, while sessions work without them.
    fn required(&self) -> bool {
        true
    }

    /// Authenticate with the method the client chose.
    ///
    /// Return an error when the method is unknown or authentication fails; the
    /// client sees it as the error response to `authenticate`.
    async fn authenticate(&self, request: &AuthenticateRequest) -> Result<(), AcpServerError>;

    /// Whether `logout` is supported. Advertised as the `auth.logout` capability.
    fn supports_logout(&self) -> bool {
        false
    }

    /// End the authenticated state. Called only when
    /// [`supports_logout`](Self::supports_logout) returns `true`.
    async fn logout(&self) -> Result<(), AcpServerError> {
        Ok(())
    }

    /// The accounts `auth/status` reports while the connection is authenticated.
    async fn accounts(&self) -> Vec<AuthAccount> {
        Vec::new()
    }
}

/// The authentication state of one ACP connection.
#[derive(Debug, Default)]
pub(crate) struct ConnectionAuth {
    /// The last state reported to the client, as [`STATE_*`] values.
    state: AtomicU8,
    /// The last state sent in an `auth/status_update` notification.
    reported: AtomicU8,
}

const STATE_UNKNOWN: u8 = 0;
const STATE_AUTHENTICATED: u8 = 1;
const STATE_UNAUTHENTICATED: u8 = 2;

impl ConnectionAuth {
    pub(crate) fn mark_authenticated(&self) {
        self.state.store(STATE_AUTHENTICATED, Ordering::SeqCst);
    }

    pub(crate) fn mark_logged_out(&self) {
        self.state.store(STATE_UNAUTHENTICATED, Ordering::SeqCst);
    }

    pub(crate) fn is_authenticated(&self) -> bool {
        self.state.load(Ordering::SeqCst) == STATE_AUTHENTICATED
    }

    /// The state `auth/status` reports on this connection.
    pub(crate) fn state(&self, authenticator: Option<&dyn AcpAuthenticator>) -> AuthState {
        match authenticator {
            None => AuthState::NotRequired,
            Some(_) if self.is_authenticated() => AuthState::Authenticated,
            Some(authenticator) if !authenticator.required() => AuthState::NotRequired,
            Some(_) => AuthState::Unauthenticated,
        }
    }

    /// Whether `state` differs from the last state reported in an
    /// `auth/status_update`, recording it as reported.
    pub(crate) fn should_report(&self, state: &AuthState) -> bool {
        let code = match state {
            AuthState::Authenticated => STATE_AUTHENTICATED,
            AuthState::Unauthenticated => STATE_UNAUTHENTICATED,
            _ => STATE_UNKNOWN,
        };
        self.reported.swap(code, Ordering::SeqCst) != code
    }

    /// Fail with `auth_required` when the authenticator requires
    /// authentication and the connection has not authenticated.
    pub(crate) fn ensure(
        &self,
        authenticator: Option<&dyn AcpAuthenticator>,
    ) -> Result<(), AcpServerError> {
        match authenticator {
            Some(authenticator) if authenticator.required() && !self.is_authenticated() => {
                Err(AcpServerError::AuthRequired)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::AuthMethodAgent;

    use super::*;

    struct Required(bool);

    #[async_trait]
    impl AcpAuthenticator for Required {
        fn methods(&self) -> Vec<AuthMethod> {
            vec![AuthMethod::Agent(AuthMethodAgent::new("token", "Token"))]
        }

        fn required(&self) -> bool {
            self.0
        }

        async fn authenticate(&self, _request: &AuthenticateRequest) -> Result<(), AcpServerError> {
            Ok(())
        }
    }

    #[test]
    fn no_authenticator_never_requires_authentication() {
        let auth = ConnectionAuth::default();
        assert!(auth.ensure(None).is_ok());
        assert_eq!(auth.state(None), AuthState::NotRequired);
    }

    #[test]
    fn a_required_authenticator_gates_until_authenticated() {
        let auth = ConnectionAuth::default();
        let authenticator = Required(true);
        assert!(matches!(auth.ensure(Some(&authenticator)), Err(AcpServerError::AuthRequired)));
        assert_eq!(auth.state(Some(&authenticator)), AuthState::Unauthenticated);
        auth.mark_authenticated();
        assert!(auth.ensure(Some(&authenticator)).is_ok());
        assert_eq!(auth.state(Some(&authenticator)), AuthState::Authenticated);
        auth.mark_logged_out();
        assert!(auth.ensure(Some(&authenticator)).is_err());
    }

    #[test]
    fn an_optional_authenticator_reports_not_required_until_used() {
        let auth = ConnectionAuth::default();
        let authenticator = Required(false);
        assert!(auth.ensure(Some(&authenticator)).is_ok());
        assert_eq!(auth.state(Some(&authenticator)), AuthState::NotRequired);
        auth.mark_authenticated();
        assert_eq!(auth.state(Some(&authenticator)), AuthState::Authenticated);
    }

    #[test]
    fn status_updates_are_reported_only_on_change() {
        let auth = ConnectionAuth::default();
        assert!(auth.should_report(&AuthState::Authenticated));
        assert!(!auth.should_report(&AuthState::Authenticated));
        assert!(auth.should_report(&AuthState::Unauthenticated));
    }
}
