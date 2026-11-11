//! AWS: `AssumeRoleWithWebIdentity` at the Security Token Service, and
//! `GetWorkloadAccessTokenForJWT` at Amazon Bedrock AgentCore Identity.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{AgentPrincipal, CredentialSecret, Url};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::exchange::{REQUEST_TIMEOUT, send, shorten};
use super::sigv4::{SigningRequest, SigningScope, sign};
use super::{AgentTokenSource, CloudProvider, FederationError};

/// Temporary AWS credentials for an assumed role.
///
/// `Debug` never prints the secret access key or the session token.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::AwsSessionCredentials;
/// use adk_core::CredentialSecret;
///
/// let credentials = AwsSessionCredentials {
///     access_key_id: "ASIAEXAMPLE".into(),
///     secret_access_key: CredentialSecret::new("wJalrXUtnFEMI"),
///     session_token: CredentialSecret::new("IQoJb3JpZ2lu"),
///     expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
///     assumed_role_arn: "arn:aws:sts::111122223333:assumed-role/cashier/session".into(),
///     subject_from_web_identity_token: "did:web:shop.example.com:agents:cashier".into(),
/// };
/// let printed = format!("{credentials:?}");
/// assert!(!printed.contains("wJalrXUtnFEMI") && !printed.contains("IQoJb3JpZ2lu"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsSessionCredentials {
    /// The access key id.
    pub access_key_id: String,
    /// The secret access key.
    pub secret_access_key: CredentialSecret,
    /// The session token, sent as `X-Amz-Security-Token`.
    pub session_token: CredentialSecret,
    /// When the credentials stop being valid.
    pub expires_at: DateTime<Utc>,
    /// The ARN of the assumed-role session.
    pub assumed_role_arn: String,
    /// The `sub` claim AWS read from the agent's token.
    pub subject_from_web_identity_token: String,
}

/// Exchanges an agent's token for temporary credentials of an IAM role
/// (`sts:AssumeRoleWithWebIdentity`).
///
/// The role's trust policy names the issuer as an IAM OIDC provider and the agent's
/// [`AgentId`](adk_core::AgentId) as the `sub` condition. The request is unsigned and needs no
/// AWS credentials. The agent's token is requested for the audience
/// [`AUDIENCE`](Self::AUDIENCE).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_auth::federation::{AgentTokenIssuer, AwsWebIdentity};
/// use adk_core::{AgentId, AgentPrincipal};
///
/// # async fn example(pem: &str) -> Result<(), Box<dyn std::error::Error>> {
/// let issuer = AgentTokenIssuer::from_rsa_pem("https://agents.example.com", "agents-1", pem)?;
/// let cashier = AgentPrincipal::new(AgentId::new("did:web:shop.example.com:agents:cashier")?);
///
/// let sts = AwsWebIdentity::new(
///     "us-east-1",
///     "arn:aws:iam::111122223333:role/cashier",
///     Arc::new(issuer),
/// );
/// let credentials = sts.assume_role(&cashier).await?;
/// assert_eq!(credentials.subject_from_web_identity_token, cashier.id.as_str());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct AwsWebIdentity {
    endpoint: String,
    role_arn: String,
    session_name: Option<String>,
    duration: Duration,
    tokens: Arc<dyn AgentTokenSource>,
    client: reqwest::Client,
}

impl AwsWebIdentity {
    /// The audience the agent's token is issued for.
    pub const AUDIENCE: &'static str = "sts.amazonaws.com";

    /// A client assuming `role_arn` at the regional endpoint
    /// `https://sts.{region}.amazonaws.com/` with tokens from `tokens`.
    pub fn new(
        region: impl AsRef<str>,
        role_arn: impl Into<String>,
        tokens: Arc<dyn AgentTokenSource>,
    ) -> Self {
        Self {
            endpoint: format!("https://sts.{}.amazonaws.com/", region.as_ref()),
            role_arn: role_arn.into(),
            session_name: None,
            duration: Duration::from_secs(3600),
            tokens,
            client: crate::http::client(REQUEST_TIMEOUT),
        }
    }

    /// Names the role session `name` (2 to 64 characters of `[A-Za-z0-9_+=,.@-]`).
    ///
    /// Without it the session is named after the agent: its id with every other character
    /// replaced by `-`, cut to the last 64.
    #[must_use]
    pub fn with_session_name(mut self, name: impl Into<String>) -> Self {
        self.session_name = Some(name.into());
        self
    }

    /// Asks for credentials valid for `duration` (one hour by default; AWS accepts 15
    /// minutes up to the role's maximum session duration).
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// Sends the request to `endpoint` instead of the regional STS endpoint, for a VPC or
    /// FIPS endpoint.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Uses `client` for every request.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Assumes the role as `agent`.
    ///
    /// # Errors
    ///
    /// Returns the token source's error when it yields no token,
    /// [`FederationError::Transport`] when STS cannot be reached,
    /// [`FederationError::Refused`] with the STS error code (`AccessDenied`,
    /// `InvalidIdentityToken`, ...) when it refuses, and
    /// [`FederationError::InvalidResponse`] when its answer cannot be read.
    pub async fn assume_role(
        &self,
        agent: &AgentPrincipal,
    ) -> Result<AwsSessionCredentials, FederationError> {
        const PROVIDER: CloudProvider = CloudProvider::Aws;
        let token = self.tokens.agent_token(agent, Self::AUDIENCE).await?;
        let session_name = match &self.session_name {
            Some(name) => name.clone(),
            None => {
                let safe: Vec<char> =
                    agent
                        .id
                        .as_str()
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || "_+=,.@-".contains(c) { c } else { '-' }
                        })
                        .collect();
                safe[safe.len().saturating_sub(64)..].iter().collect()
            }
        };
        let duration = self.duration.as_secs().to_string();
        let form = [
            ("Action", "AssumeRoleWithWebIdentity"),
            ("Version", "2011-06-15"),
            ("RoleArn", self.role_arn.as_str()),
            ("RoleSessionName", session_name.as_str()),
            ("WebIdentityToken", token.secret.expose()),
            ("DurationSeconds", duration.as_str()),
        ];
        let request =
            self.client.post(&self.endpoint).header("accept", "application/json").form(&form);
        let (status, _, body) = send(PROVIDER, &self.endpoint, request).await?;
        if !(200..300).contains(&status) {
            return Err(refusal(PROVIDER, status, None, &body));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Envelope {
            assume_role_with_web_identity_response: Response,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Response {
            assume_role_with_web_identity_result: Outcome,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Outcome {
            credentials: Credentials,
            assumed_role_user: AssumedRoleUser,
            subject_from_web_identity_token: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Credentials {
            access_key_id: String,
            secret_access_key: String,
            session_token: String,
            expiration: serde_json::Value,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct AssumedRoleUser {
            arn: String,
        }

        let invalid =
            |reason: String| FederationError::InvalidResponse { provider: PROVIDER, reason };
        let envelope: Envelope = serde_json::from_str(&body).map_err(|error| {
            // serde_json reports a position and a field name, never a value.
            invalid(format!("the AssumeRoleWithWebIdentity response is not as documented: {error}"))
        })?;
        let outcome =
            envelope.assume_role_with_web_identity_response.assume_role_with_web_identity_result;
        // The JSON form carries `Expiration` as epoch seconds; an ISO 8601 string is read too.
        let expires_at = match &outcome.credentials.expiration {
            serde_json::Value::Number(seconds) => {
                seconds.as_f64().and_then(|seconds| DateTime::from_timestamp(seconds as i64, 0))
            }
            serde_json::Value::String(text) => {
                DateTime::parse_from_rfc3339(text).ok().map(|at| at.with_timezone(&Utc))
            }
            _ => None,
        }
        .ok_or_else(|| invalid("the credentials carry no readable `Expiration`".into()))?;
        tracing::debug!(
            agent.id = %agent.id,
            aws.assumed_role = %outcome.assumed_role_user.arn,
            "aws role assumed with the agent's token"
        );
        Ok(AwsSessionCredentials {
            access_key_id: outcome.credentials.access_key_id,
            secret_access_key: CredentialSecret::new(outcome.credentials.secret_access_key),
            session_token: CredentialSecret::new(outcome.credentials.session_token),
            expires_at,
            assumed_role_arn: outcome.assumed_role_user.arn,
            subject_from_web_identity_token: outcome.subject_from_web_identity_token,
        })
    }
}

/// The refusal an AWS JSON error body describes.
///
/// STS answers `{"Error": {"Code", "Message"}}`; REST-JSON services answer `{"message"}` with
/// the error type in the `x-amzn-errortype` header.
fn refusal(
    provider: CloudProvider,
    status: u16,
    error_type: Option<&str>,
    body: &str,
) -> FederationError {
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let text = |value: &serde_json::Value| value.as_str().map(str::to_string);
    let code = text(&json["Error"]["Code"])
        .or_else(|| error_type.map(|kind| kind.split(':').next().unwrap_or(kind).to_string()))
        .or_else(|| text(&json["__type"]))
        .unwrap_or_else(|| format!("http_{status}"));
    let message = text(&json["Error"]["Message"])
        .or_else(|| text(&json["message"]))
        .or_else(|| text(&json["Message"]))
        .unwrap_or_else(|| body.to_string());
    FederationError::Refused { provider, status, code, message: shorten(&message) }
}

/// An Amazon Bedrock AgentCore workload access token: an opaque value naming both the
/// workload and the identity in the token it was exchanged for.
///
/// AgentCore states no lifetime for it. `Debug` never prints the token.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::WorkloadAccessToken;
/// use adk_core::CredentialSecret;
///
/// let token = WorkloadAccessToken {
///     workload_name: "cashier".into(),
///     secret: CredentialSecret::new("opaque-value"),
/// };
/// assert!(!format!("{token:?}").contains("opaque-value"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadAccessToken {
    /// The workload identity the token was issued for.
    pub workload_name: String,
    /// The opaque token.
    pub secret: CredentialSecret,
}

/// Exchanges a JWT for a workload access token at Amazon Bedrock AgentCore Identity
/// (`bedrock-agentcore:GetWorkloadAccessTokenForJWT`).
///
/// The request is signed with Signature Version 4 under temporary credentials, such as
/// those [`AwsWebIdentity::assume_role`] returns. AgentCore fetches the keys of the JWT's
/// issuer itself.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_auth::federation::{AgentCoreIdentity, AgentTokenIssuer, AwsWebIdentity};
/// use adk_core::{AgentId, AgentPrincipal};
///
/// # async fn example(pem: &str) -> Result<(), Box<dyn std::error::Error>> {
/// let issuer =
///     Arc::new(AgentTokenIssuer::from_rsa_pem("https://agents.example.com", "agents-1", pem)?);
/// let cashier = AgentPrincipal::new(AgentId::new("did:web:shop.example.com:agents:cashier")?);
///
/// let credentials =
///     AwsWebIdentity::new("us-east-1", "arn:aws:iam::111122223333:role/cashier", issuer.clone())
///         .assume_role(&cashier)
///         .await?;
/// let own_token = issuer.issue(&cashier, AwsWebIdentity::AUDIENCE).await?;
/// let workload = AgentCoreIdentity::new("us-east-1")
///     .workload_access_token(&credentials, "cashier", &own_token.secret)
///     .await?;
/// assert_eq!(workload.workload_name, "cashier");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct AgentCoreIdentity {
    region: String,
    endpoint: String,
    client: reqwest::Client,
}

impl AgentCoreIdentity {
    const SERVICE: &'static str = "bedrock-agentcore";
    const PATH: &'static str = "/identities/GetWorkloadAccessTokenForJWT";

    /// A client of the data plane at `https://bedrock-agentcore.{region}.amazonaws.com`.
    pub fn new(region: impl Into<String>) -> Self {
        let region = region.into();
        Self {
            endpoint: format!("https://{}.{region}.amazonaws.com", Self::SERVICE),
            region,
            client: crate::http::client(REQUEST_TIMEOUT),
        }
    }

    /// Sends the request to `endpoint` (scheme, host and optional port) instead of the
    /// regional endpoint. Requests stay signed for the client's region.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into().trim_end_matches('/').to_string();
        self
    }

    /// Uses `client` for every request.
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Exchanges `user_token`, a JWT such as the agent's own token, for a workload access
    /// token of the workload identity `workload_name`.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::InvalidConfig`] when the endpoint is not a URL,
    /// [`FederationError::Transport`] when AgentCore cannot be reached,
    /// [`FederationError::Refused`] with the exception name (`AccessDeniedException`,
    /// `UnauthorizedException`, ...) when it refuses, and
    /// [`FederationError::InvalidResponse`] when its answer holds no token.
    pub async fn workload_access_token(
        &self,
        credentials: &AwsSessionCredentials,
        workload_name: &str,
        user_token: &CredentialSecret,
    ) -> Result<WorkloadAccessToken, FederationError> {
        const PROVIDER: CloudProvider = CloudProvider::AgentCore;
        let url = format!("{}{}", self.endpoint, Self::PATH);
        let host = Url::parse(&url)
            .ok()
            .and_then(|url| {
                let host = url.host_str()?.to_string();
                Some(url.port().map_or(host.clone(), |port| format!("{host}:{port}")))
            })
            .ok_or_else(|| FederationError::InvalidConfig {
                reason: format!(
                    "AgentCore endpoint '{}' is not a URL; use a value such as 'https://bedrock-agentcore.us-east-1.amazonaws.com'",
                    self.endpoint
                ),
            })?;
        let body =
            json!({ "workloadName": workload_name, "userToken": user_token.expose() }).to_string();
        let signed = sign(
            &SigningRequest {
                method: "POST",
                host: &host,
                path: Self::PATH,
                query: "",
                headers: &[("content-type", "application/json")],
                payload: body.as_bytes(),
            },
            &SigningScope {
                access_key_id: &credentials.access_key_id,
                secret_access_key: credentials.secret_access_key.expose(),
                session_token: Some(credentials.session_token.expose()),
                region: &self.region,
                service: Self::SERVICE,
                time: Utc::now(),
            },
        );
        let request = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-amz-date", signed.amz_date)
            .header("x-amz-security-token", credentials.session_token.expose())
            .header("authorization", signed.authorization)
            .body(body);
        let (status, headers, answer) = send(PROVIDER, &url, request).await?;
        if !(200..300).contains(&status) {
            let error_type = headers.get("x-amzn-errortype").and_then(|value| value.to_str().ok());
            return Err(refusal(PROVIDER, status, error_type, &answer));
        }
        let json: serde_json::Value = serde_json::from_str(&answer).unwrap_or_default();
        let secret = json["workloadAccessToken"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| FederationError::InvalidResponse {
                provider: PROVIDER,
                reason: "the response has no `workloadAccessToken`".into(),
            })?;
        tracing::debug!(aws.workload = workload_name, "agentcore workload access token issued");
        Ok(WorkloadAccessToken {
            workload_name: workload_name.to_string(),
            secret: CredentialSecret::new(secret),
        })
    }
}
