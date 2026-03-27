//! ACP experimental surfaces gated behind `acp-experimental`.
//!
//! This module carries the unreleased discovery document, signed webhook
//! receiver, and delegated-authentication flow while keeping the stable ACP
//! builder unchanged.

mod delegate_authentication;
mod discovery;
mod server;
mod webhook;

pub use delegate_authentication::{
    AcpAuthenticateDelegateAuthenticationSessionCommand,
    AcpCreateDelegateAuthenticationSessionCommand, AcpDelegateAuthenticationAcquirerDetails,
    AcpDelegateAuthenticationAction, AcpDelegateAuthenticationActionType,
    AcpDelegateAuthenticationAddress, AcpDelegateAuthenticationAmount,
    AcpDelegateAuthenticationAuthenticateRequest, AcpDelegateAuthenticationBrowserInfo,
    AcpDelegateAuthenticationChallengeAction, AcpDelegateAuthenticationChallengeDetails,
    AcpDelegateAuthenticationChallengePreference, AcpDelegateAuthenticationChannel,
    AcpDelegateAuthenticationChannelType, AcpDelegateAuthenticationCreateRequest,
    AcpDelegateAuthenticationFingerprintAction, AcpDelegateAuthenticationFingerprintCompletion,
    AcpDelegateAuthenticationFlowPreference, AcpDelegateAuthenticationFlowType,
    AcpDelegateAuthenticationPaymentMethod, AcpDelegateAuthenticationPaymentMethodType,
    AcpDelegateAuthenticationResult, AcpDelegateAuthenticationService,
    AcpDelegateAuthenticationSession, AcpDelegateAuthenticationSessionLookup,
    AcpDelegateAuthenticationSessionState, AcpDelegateAuthenticationSessionStatus,
    AcpDelegateAuthenticationSessionWithResult, AcpDelegateAuthenticationShopperDetails,
};
pub use discovery::{
    AcpDiscoveryCapabilities, AcpDiscoveryDocument, AcpDiscoveryExtension,
    AcpDiscoveryInterventionType, AcpDiscoveryProtocol, AcpDiscoveryService, AcpDiscoveryTransport,
};
pub use server::AcpExperimentalRouterBuilder;
pub use webhook::AcpMerchantWebhookVerificationConfig;
