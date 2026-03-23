use adk_core::Result;
use adk_core::identity::AdkIdentity;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::ProtocolExtensions;
use crate::kernel::CommerceContext;

/// ACP delegated-authentication session creation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationCreateRequest {
    /// Merchant identifier recognized by the authentication provider.
    pub merchant_id: String,
    /// Optional acquirer metadata used to construct AReq payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquirer_details: Option<AcpDelegateAuthenticationAcquirerDetails>,
    /// Card data used for the delegated-authentication session.
    pub payment_method: AcpDelegateAuthenticationPaymentMethod,
    /// Amount used for authentication and eventual authorization correlation.
    pub amount: AcpDelegateAuthenticationAmount,
    /// Optional browser-channel metadata for 3DS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<AcpDelegateAuthenticationChannel>,
    /// Optional ACP checkout-session correlation handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_session_id: Option<String>,
    /// Optional merchant request for challenge or frictionless preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_preference: Option<AcpDelegateAuthenticationFlowPreference>,
    /// Optional callback URL for challenge completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_notification_url: Option<String>,
    /// Optional shopper identity details used during authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shopper_details: Option<AcpDelegateAuthenticationShopperDetails>,
}

/// ACP delegated-authentication session authentication request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationAuthenticateRequest {
    /// Outcome of the 3DS method fingerprint attempt.
    pub fingerprint_completion: AcpDelegateAuthenticationFingerprintCompletion,
    /// Optional deferred browser metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<AcpDelegateAuthenticationChannel>,
    /// Optional ACP checkout-session correlation handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_session_id: Option<String>,
    /// Optional callback URL for challenge completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_notification_url: Option<String>,
    /// Optional shopper identity details used during authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shopper_details: Option<AcpDelegateAuthenticationShopperDetails>,
}

/// Card type supported by ACP delegated-authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpDelegateAuthenticationPaymentMethodType {
    /// Card PAN input.
    #[serde(rename = "card")]
    Card,
}

/// Delegated-authentication payment method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationPaymentMethod {
    /// Supported method family.
    pub r#type: AcpDelegateAuthenticationPaymentMethodType,
    /// Primary account number.
    pub number: String,
    /// Two-digit expiry month.
    pub exp_month: String,
    /// Four-digit expiry year.
    pub exp_year: String,
    /// Cardholder name.
    pub name: String,
}

/// Delegated-authentication amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationAmount {
    /// Amount in minor units.
    pub value: i64,
    /// Uppercase ISO-4217 currency code.
    pub currency: String,
}

/// Optional acquirer metadata for 3DS request construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationAcquirerDetails {
    /// Acquirer BIN.
    pub acquirer_bin: String,
    /// Two-letter ISO-3166-1 country code.
    pub acquirer_country: String,
    /// Merchant identifier assigned by the acquirer.
    pub acquirer_merchant_id: String,
    /// Merchant display name for the acquirer.
    pub merchant_name: String,
    /// Optional 3DS requestor identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requestor_id: Option<String>,
}

/// Channel type supported by delegated-authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpDelegateAuthenticationChannelType {
    /// Browser-based 3DS.
    #[serde(rename = "browser")]
    Browser,
}

/// Delegated-authentication channel metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationChannel {
    /// Supported channel family.
    pub r#type: AcpDelegateAuthenticationChannelType,
    /// Browser telemetry required for 3DS browser flows.
    pub browser: AcpDelegateAuthenticationBrowserInfo,
}

/// Browser metadata captured for 3DS browser flows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationBrowserInfo {
    /// Raw HTTP `Accept` header.
    pub accept_header: String,
    /// Browser IP address.
    pub ip_address: String,
    /// Whether JavaScript is enabled.
    pub javascript_enabled: bool,
    /// Browser language tag.
    pub language: String,
    /// Browser user agent string.
    pub user_agent: String,
    /// Optional screen color depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_depth: Option<i64>,
    /// Optional Java support flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_enabled: Option<bool>,
    /// Optional screen height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_height: Option<i64>,
    /// Optional screen width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_width: Option<i64>,
    /// Optional timezone offset in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone_offset: Option<i64>,
}

/// Flow preference type requested by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpDelegateAuthenticationFlowType {
    /// Challenge-preferred flow.
    #[serde(rename = "challenge")]
    Challenge,
    /// Frictionless-preferred flow.
    #[serde(rename = "frictionless")]
    Frictionless,
}

/// Challenge preference subtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpDelegateAuthenticationChallengePreference {
    /// Challenge is mandatory.
    #[serde(rename = "mandated")]
    Mandated,
    /// Challenge is preferred but not required.
    #[serde(rename = "preferred")]
    Preferred,
}

/// Client flow preference for delegated-authentication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationFlowPreference {
    /// Requested flow family.
    pub r#type: AcpDelegateAuthenticationFlowType,
    /// Optional challenge preference details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<AcpDelegateAuthenticationChallengeDetails>,
}

/// Optional challenge preference details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationChallengeDetails {
    /// Requested challenge preference subtype.
    pub r#type: AcpDelegateAuthenticationChallengePreference,
}

/// Shopper metadata provided to the authentication provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationShopperDetails {
    /// Shopper name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Shopper email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Shopper phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// Shopper billing or home address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<AcpDelegateAuthenticationAddress>,
}

/// Physical address carried in delegated-authentication requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationAddress {
    /// Recipient name.
    pub name: String,
    /// Address line one.
    pub line_one: String,
    /// Optional address line two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_two: Option<String>,
    /// City name.
    pub city: String,
    /// State or province.
    pub state: String,
    /// Two-letter ISO-3166-1 country code.
    pub country: String,
    /// Postal code.
    pub postal_code: String,
}

/// Fingerprint completion status submitted by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcpDelegateAuthenticationFingerprintCompletion {
    /// 3DS method completed successfully.
    #[serde(rename = "Y")]
    Completed,
    /// 3DS method timed out or was not completed.
    #[serde(rename = "N")]
    NotCompleted,
    /// 3DS method was unavailable.
    #[serde(rename = "U")]
    Unavailable,
}

/// Delegated-authentication session status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpDelegateAuthenticationSessionStatus {
    /// Further browser interaction is required.
    ActionRequired,
    /// Session is waiting for completion.
    Pending,
    /// Authentication is not supported for this card or channel.
    NotSupported,
    /// Authentication completed successfully.
    Authenticated,
    /// Authentication attempted but liability shift may vary.
    Attempted,
    /// Authentication completed negatively.
    NotAuthenticated,
    /// Authentication was rejected.
    Rejected,
    /// Authentication service unavailable.
    Unavailable,
    /// Session expired.
    Expired,
    /// Challenge was abandoned before completion.
    ChallengeAbandoned,
}

/// Delegated-authentication action type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpDelegateAuthenticationActionType {
    /// Browser fingerprint is required.
    Fingerprint,
    /// Browser challenge is required.
    Challenge,
}

/// Browser action required by the authentication provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationAction {
    /// Action family.
    pub r#type: AcpDelegateAuthenticationActionType,
    /// Fingerprint details for 3DS method execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<AcpDelegateAuthenticationFingerprintAction>,
    /// Challenge details for ACS browser interaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<AcpDelegateAuthenticationChallengeAction>,
}

/// 3DS fingerprint action payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationFingerprintAction {
    /// Issuer fingerprint endpoint.
    pub three_ds_method_url: String,
    /// 3DS server transaction identifier.
    pub three_ds_server_trans_id: String,
}

/// 3DS challenge action payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationChallengeAction {
    /// ACS challenge endpoint.
    pub acs_url: String,
    /// ACS transaction identifier.
    pub acs_trans_id: String,
    /// 3DS server transaction identifier.
    pub three_ds_server_trans_id: String,
    /// 3DS message version.
    pub message_version: String,
}

/// Finalized 3DS authentication result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationResult {
    /// EMV 3DS transaction status.
    pub trans_status: String,
    /// Optional ECI value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electronic_commerce_indicator: Option<String>,
    /// Optional cryptogram value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub three_ds_cryptogram: Option<String>,
    /// Directory-server transaction identifier.
    pub transaction_id: String,
    /// 3DS server transaction identifier.
    pub three_ds_server_trans_id: String,
    /// 3DS protocol version.
    pub version: String,
    /// Optional authentication value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication_value: Option<String>,
    /// Optional transaction-status reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trans_status_reason: Option<String>,
    /// Optional shopper-facing message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardholder_info: Option<String>,
}

/// Wire response for create and authenticate delegated-authentication routes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationSession {
    /// Session identifier used for follow-up operations.
    pub authentication_session_id: String,
    /// Current delegated-authentication status.
    pub status: AcpDelegateAuthenticationSessionStatus,
    /// Optional browser action required to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AcpDelegateAuthenticationAction>,
}

/// Wire response for delegated-authentication session retrieval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpDelegateAuthenticationSessionWithResult {
    /// Session identifier used for follow-up operations.
    pub authentication_session_id: String,
    /// Current delegated-authentication status.
    pub status: AcpDelegateAuthenticationSessionStatus,
    /// Optional browser action required to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AcpDelegateAuthenticationAction>,
    /// Optional final authentication result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication_result: Option<AcpDelegateAuthenticationResult>,
}

/// Backend-facing delegated-authentication session state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDelegateAuthenticationSessionState {
    /// Session identifier used for follow-up operations.
    pub authentication_session_id: String,
    /// Current delegated-authentication status.
    pub status: AcpDelegateAuthenticationSessionStatus,
    /// Optional browser action required to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AcpDelegateAuthenticationAction>,
    /// Optional final authentication result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication_result: Option<AcpDelegateAuthenticationResult>,
    /// Optional checkout-session correlation handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_session_id: Option<String>,
    /// Optional lossless protocol data retained outside canonical projections.
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

impl AcpDelegateAuthenticationSessionState {
    /// Returns the wire response used by create and authenticate routes.
    #[must_use]
    pub fn to_session(&self) -> AcpDelegateAuthenticationSession {
        AcpDelegateAuthenticationSession {
            authentication_session_id: self.authentication_session_id.clone(),
            status: self.status,
            action: self.action.clone(),
        }
    }

    /// Returns the wire response used by retrieve routes.
    #[must_use]
    pub fn to_session_with_result(&self) -> AcpDelegateAuthenticationSessionWithResult {
        AcpDelegateAuthenticationSessionWithResult {
            authentication_session_id: self.authentication_session_id.clone(),
            status: self.status,
            action: self.action.clone(),
            authentication_result: self.authentication_result.clone(),
        }
    }
}

/// Backend-facing create command for delegated-authentication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpCreateDelegateAuthenticationSessionCommand {
    /// Canonical commerce context correlated to the auth session.
    pub context: CommerceContext,
    /// ACP create request body.
    pub request: AcpDelegateAuthenticationCreateRequest,
}

/// Backend-facing authenticate command for delegated-authentication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAuthenticateDelegateAuthenticationSessionCommand {
    /// Canonical commerce context correlated to the auth session.
    pub context: CommerceContext,
    /// Session identifier from the route path.
    pub authentication_session_id: String,
    /// ACP authenticate request body.
    pub request: AcpDelegateAuthenticationAuthenticateRequest,
}

/// Backend-facing session lookup for delegated-authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDelegateAuthenticationSessionLookup {
    /// Session identifier from the route path.
    pub authentication_session_id: String,
    /// Optional authenticated ADK session identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<AdkIdentity>,
}

/// Backend-facing delegated-authentication operations.
#[async_trait]
pub trait AcpDelegateAuthenticationService: Send + Sync {
    /// Creates a delegated-authentication session.
    async fn create_authentication_session(
        &self,
        command: AcpCreateDelegateAuthenticationSessionCommand,
    ) -> Result<AcpDelegateAuthenticationSessionState>;

    /// Continues a delegated-authentication session after fingerprinting.
    async fn authenticate_session(
        &self,
        command: AcpAuthenticateDelegateAuthenticationSessionCommand,
    ) -> Result<AcpDelegateAuthenticationSessionState>;

    /// Retrieves the current delegated-authentication session state.
    async fn get_authentication_session(
        &self,
        lookup: AcpDelegateAuthenticationSessionLookup,
    ) -> Result<Option<AcpDelegateAuthenticationSessionState>>;
}
