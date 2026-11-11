//! Inbound: a cloud's agent token, validated and read as an [`AgentPrincipal`].
//!
//! Signature, issuer, audience and lifetime are checked by a
//! [`TokenValidator`](crate::sso::TokenValidator) of the `sso` module; an
//! [`AgentClaimsProfile`] then names the agent from the validated claims.

use std::fmt;
use std::sync::Arc;

use adk_core::{AgentId, AgentPrincipal};

use super::FederationError;
use crate::sso::{JwtValidator, OidcProvider, TokenClaims, TokenValidator};

/// Names the agent a cloud's validated token identifies.
///
/// One profile per identity system: [`EntraAgentProfile`] and [`GoogleAgentProfile`] are
/// provided. A profile reads claims only; the token is validated before it is called.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentClaimsProfile, FederationError};
/// use adk_auth::sso::TokenClaims;
/// use adk_core::{AgentId, AgentPrincipal};
///
/// /// An identity system whose tokens carry the agent's URI in `sub` and its tenant in `org`.
/// #[derive(Debug)]
/// struct HouseProfile;
///
/// impl AgentClaimsProfile for HouseProfile {
///     fn principal(&self, claims: &TokenClaims) -> Result<AgentPrincipal, FederationError> {
///         let id = AgentId::new(claims.sub.as_str()).map_err(|error| FederationError::Claim {
///             claim: "sub".into(),
///             reason: error.to_string(),
///         })?;
///         let principal = AgentPrincipal::new(id);
///         Ok(match claims.get_custom::<String>("org") {
///             Some(org) => principal.with_tenant(org),
///             None => principal,
///         })
///     }
/// }
/// ```
pub trait AgentClaimsProfile: fmt::Debug + Send + Sync {
    /// The principal `claims` identify.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::Claim`] when a claim the profile needs is absent or
    /// malformed.
    fn principal(&self, claims: &TokenClaims) -> Result<AgentPrincipal, FederationError>;
}

/// Reads a Microsoft Entra access token as an agent.
///
/// | Principal field | Claim |
/// |-----------------|-------|
/// | `id` | `urn:entra:{tid}:agent:{azp}`; `appid` stands in for `azp` on a v1.0 token |
/// | `tenant` | `tid` |
/// | `class` | `xms_par_app_azp`, the agent identity blueprint, when present |
/// | `attributes` | `entra.tenant_id`, `entra.agent_identity_id`, and, when the claim is present, `entra.object_id` (`oid`), `entra.blueprint_app_id` (`xms_par_app_azp`), `entra.token_version` (`ver`), `entra.identity_type` (`idtyp`), `entra.actor_facets` (`xms_act_fct`), `entra.subject_facets` (`xms_sub_fct`) |
///
/// `entra.agent_identity_facet` is `true` when `xms_act_fct` or `xms_sub_fct` lists facet
/// `11`, which marks an agent identity. Entra omits both claims, `xms_par_app_azp` and
/// `idtyp` from a token for a custom audience unless optional claims are configured, so
/// they are recorded as evidence and never required: authorize by the principal's `id`.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentClaimsProfile, EntraAgentProfile};
/// use adk_auth::sso::TokenClaims;
///
/// let claims: TokenClaims = serde_json::from_value(serde_json::json!({
///     "sub": "8f2c6a1e-0000-4000-8000-000000000003",
///     "tid": "8f2c6a1e-0000-4000-8000-000000000001",
///     "azp": "8f2c6a1e-0000-4000-8000-000000000003",
///     "xms_par_app_azp": "8f2c6a1e-0000-4000-8000-000000000002",
///     "xms_act_fct": "3 11 9",
/// }))?;
/// let principal = EntraAgentProfile.principal(&claims)?;
/// assert_eq!(
///     principal.id.as_str(),
///     "urn:entra:8f2c6a1e-0000-4000-8000-000000000001:agent:8f2c6a1e-0000-4000-8000-000000000003"
/// );
/// assert_eq!(principal.class.as_deref(), Some("8f2c6a1e-0000-4000-8000-000000000002"));
/// assert_eq!(principal.attributes["entra.agent_identity_facet"], "true");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntraAgentProfile;

impl AgentClaimsProfile for EntraAgentProfile {
    fn principal(&self, claims: &TokenClaims) -> Result<AgentPrincipal, FederationError> {
        let text = |name: &str| {
            claims.custom.get(name).and_then(serde_json::Value::as_str).filter(|v| !v.is_empty())
        };
        let missing = |claim: &str, reason: &str| FederationError::Claim {
            claim: claim.to_string(),
            reason: reason.to_string(),
        };
        let agent = text("azp").or_else(|| text("appid")).ok_or_else(|| {
            missing("azp", "neither `azp` (v2.0) nor `appid` (v1.0) names the calling application")
        })?;
        let tenant = claims
            .tid
            .as_deref()
            .filter(|tid| !tid.is_empty())
            .ok_or_else(|| missing("tid", "the token names no tenant"))?;
        let id = AgentId::new(format!("urn:entra:{tenant}:agent:{agent}"))
            .map_err(|error| missing("azp", &error.to_string()))?;

        let mut principal = AgentPrincipal::new(id)
            .with_tenant(tenant)
            .with_attribute("entra.tenant_id", tenant)
            .with_attribute("entra.agent_identity_id", agent);
        if let Some(blueprint) = text("xms_par_app_azp") {
            principal = principal.with_class(blueprint);
        }
        for (attribute, claim) in [
            ("entra.object_id", "oid"),
            ("entra.blueprint_app_id", "xms_par_app_azp"),
            ("entra.token_version", "ver"),
            ("entra.identity_type", "idtyp"),
            ("entra.actor_facets", "xms_act_fct"),
            ("entra.subject_facets", "xms_sub_fct"),
        ] {
            if let Some(value) = text(claim) {
                principal = principal.with_attribute(attribute, value);
            }
        }
        let agent_facet = ["xms_act_fct", "xms_sub_fct"]
            .into_iter()
            .filter_map(text)
            .any(|facets| facets.split_whitespace().any(|facet| facet == "11"));
        if agent_facet {
            principal = principal.with_attribute("entra.agent_identity_facet", "true");
        }
        Ok(principal)
    }
}

/// Reads the identity token of an agent hosted on Google Cloud with agent identity.
///
/// | Principal field | Claim |
/// |-----------------|-------|
/// | `id` | `sub`, the agent's SPIFFE id, unchanged |
/// | `tenant` | The SPIFFE trust domain, such as `agents.global.org-{organization}.system.id.goog` |
/// | `attributes` | From a `sub` path of the form `/resources/{service}/projects/{number}/locations/{location}/{type}/{id}`: `google.service`, `google.project_number`, `google.location`, `google.resource_type`, `google.resource_id` |
///
/// `google.bound_token` is `true` when the token carries a `cnf` claim, and
/// `google.cnf_x5t_s256` then holds its `x5t#S256` certificate thumbprint. A bound token is
/// not required. The trust domain must be the workload identity pool that issued the token
/// (the last segment of `iss`), so a pool cannot name agents of another.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentClaimsProfile, GoogleAgentProfile};
/// use adk_auth::sso::TokenClaims;
///
/// let trust_domain = "agents.global.org-123456789012.system.id.goog";
/// let subject = format!(
///     "spiffe://{trust_domain}/resources/aiplatform/projects/987654321098/locations/us-central1/reasoningEngines/4242"
/// );
/// let claims: TokenClaims = serde_json::from_value(serde_json::json!({
///     "iss": format!(
///         "https://sts.googleapis.com/v1/organizations/123456789012/locations/global/workloadIdentityPools/{trust_domain}"
///     ),
///     "sub": subject,
///     "aud": ["https://shop.example.com"],
/// }))?;
/// let principal = GoogleAgentProfile.principal(&claims)?;
/// assert_eq!(principal.id.as_str(), subject);
/// assert_eq!(principal.tenant.as_deref(), Some(trust_domain));
/// assert_eq!(principal.attributes["google.resource_type"], "reasoningEngines");
/// assert_eq!(principal.attributes["google.bound_token"], "false");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GoogleAgentProfile;

impl AgentClaimsProfile for GoogleAgentProfile {
    fn principal(&self, claims: &TokenClaims) -> Result<AgentPrincipal, FederationError> {
        let invalid = |reason: String| FederationError::Claim { claim: "sub".to_string(), reason };
        let (trust_domain, path) = claims
            .sub
            .strip_prefix("spiffe://")
            .map(|rest| rest.split_once('/').unwrap_or((rest, "")))
            .filter(|(trust_domain, _)| !trust_domain.is_empty())
            .ok_or_else(|| {
                invalid("it is not a SPIFFE id (`spiffe://{trust domain}/...`)".into())
            })?;
        let pool = claims.iss.rsplit('/').next().unwrap_or_default();
        if pool != trust_domain {
            return Err(invalid(format!(
                "its trust domain '{trust_domain}' is not the workload identity pool '{pool}' that issued the token"
            )));
        }
        let id = AgentId::new(claims.sub.as_str()).map_err(|error| invalid(error.to_string()))?;

        let mut principal = AgentPrincipal::new(id).with_tenant(trust_domain);
        if let ["resources", service, "projects", project, "locations", location, kind, resource] =
            path.split('/').collect::<Vec<_>>()[..]
        {
            principal = principal
                .with_attribute("google.service", service)
                .with_attribute("google.project_number", project)
                .with_attribute("google.location", location)
                .with_attribute("google.resource_type", kind)
                .with_attribute("google.resource_id", resource);
        }
        let confirmation = claims.custom.get("cnf");
        principal =
            principal.with_attribute("google.bound_token", confirmation.is_some().to_string());
        if let Some(thumbprint) =
            confirmation.and_then(|cnf| cnf.get("x5t#S256")).and_then(serde_json::Value::as_str)
        {
            principal = principal.with_attribute("google.cnf_x5t_s256", thumbprint);
        }
        Ok(principal)
    }
}

/// Validates a cloud's agent token and names the agent it identifies.
///
/// # Example
///
/// ```rust,no_run
/// use adk_auth::federation::InboundAgentVerifier;
///
/// # async fn example(bearer: &str) -> Result<(), Box<dyn std::error::Error>> {
/// let verifier = InboundAgentVerifier::entra(
///     "8f2c6a1e-0000-4000-8000-000000000001",
///     "8f2c6a1e-0000-4000-8000-000000000002",
/// )?;
/// let agent = verifier.verify(bearer).await?;
/// tracing::info!(agent.id = %agent.id, "request from an entra agent identity");
/// # Ok(())
/// # }
/// ```
pub struct InboundAgentVerifier {
    validator: Arc<dyn TokenValidator>,
    profile: Arc<dyn AgentClaimsProfile>,
}

impl fmt::Debug for InboundAgentVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboundAgentVerifier")
            .field("issuer", &self.validator.issuer())
            .field("profile", &self.profile)
            .finish()
    }
}

impl InboundAgentVerifier {
    /// A verifier validating with `validator` and naming the agent with `profile`.
    ///
    /// Configure the validator for RS256 alone: [`JwtValidator`] applies the first
    /// algorithm it is given.
    pub fn new(validator: Arc<dyn TokenValidator>, profile: Arc<dyn AgentClaimsProfile>) -> Self {
        Self { validator, profile }
    }

    /// A verifier for v2.0 access tokens Microsoft Entra issues in tenant `tenant_id` for
    /// `audience`: the application id of this service, which is the `aud` claim of a v2.0
    /// token addressed to it.
    ///
    /// Keys come from `https://login.microsoftonline.com/{tenant_id}/discovery/v2.0/keys`;
    /// the issuer is `https://login.microsoftonline.com/{tenant_id}/v2.0`. For v1.0 tokens
    /// (issuer `https://sts.windows.net/{tenant_id}/`) pass a [`JwtValidator`] and
    /// [`EntraAgentProfile`] to [`new`](Self::new).
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::InvalidConfig`] when `tenant_id` is not one tenant's id or
    /// domain, or `audience` is empty.
    pub fn entra(
        tenant_id: impl AsRef<str>,
        audience: impl Into<String>,
    ) -> Result<Self, FederationError> {
        let tenant = tenant_id.as_ref();
        if tenant.is_empty()
            || tenant.contains(['/', '?', '#'])
            || ["common", "organizations", "consumers"].contains(&tenant)
        {
            return Err(FederationError::InvalidConfig {
                reason: format!(
                    "'{tenant}' does not name one Entra tenant; pass the tenant's id or verified domain"
                ),
            });
        }
        let audience = audience.into();
        if audience.is_empty() {
            return Err(FederationError::InvalidConfig {
                reason: "the audience is empty; pass this service's application id".into(),
            });
        }
        let validator = JwtValidator::builder()
            .issuer(format!("https://login.microsoftonline.com/{tenant}/v2.0"))
            .audience(audience)
            .jwks_uri(format!("https://login.microsoftonline.com/{tenant}/discovery/v2.0/keys"))
            .algorithm(jsonwebtoken::Algorithm::RS256)
            .build()?;
        Ok(Self::new(Arc::new(validator), Arc::new(EntraAgentProfile)))
    }

    /// A verifier for identity tokens Google Cloud issues to hosted agents, addressed to
    /// `audience`.
    ///
    /// `issuer` is the agents' workload identity pool, such as
    /// `https://sts.googleapis.com/v1/organizations/{organization}/locations/global/workloadIdentityPools/agents.global.org-{organization}.system.id.goog`;
    /// its discovery document is fetched once, here, to find the keys.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::Token`] when the discovery document cannot be fetched or
    /// names another issuer.
    pub async fn google(
        issuer: impl Into<String>,
        audience: impl Into<String>,
    ) -> Result<Self, FederationError> {
        let validator = OidcProvider::from_discovery(issuer, audience).await?;
        Ok(Self::new(Arc::new(validator), Arc::new(GoogleAgentProfile)))
    }

    /// Validates `token` and returns the agent it identifies.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::Token`] when the signature, issuer, audience or lifetime
    /// is not valid or the keys cannot be fetched, and [`FederationError::Claim`] when the
    /// profile cannot name an agent from the claims.
    pub async fn verify(&self, token: &str) -> Result<AgentPrincipal, FederationError> {
        let claims = self.validator.validate(token).await?;
        let principal = self.profile.principal(&claims)?;
        tracing::debug!(
            agent.id = %principal.id,
            token.issuer = %claims.iss,
            "inbound agent token verified"
        );
        Ok(principal)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::federation::{AgentTokenIssuer, RsaTokenSigner};
    use crate::sso::TokenError;

    const TENANT: &str = "8f2c6a1e-0000-4000-8000-000000000001";
    const BLUEPRINT: &str = "8f2c6a1e-0000-4000-8000-000000000002";
    const AGENT: &str = "8f2c6a1e-0000-4000-8000-000000000003";
    const ENTRA_ISSUER: &str =
        "https://login.microsoftonline.com/8f2c6a1e-0000-4000-8000-000000000001/v2.0";
    const TRUST_DOMAIN: &str = "agents.global.org-123456789012.system.id.goog";

    /// Stands in for a cloud's signing key; generated once per test binary.
    fn cloud() -> &'static AgentTokenIssuer {
        static CLOUD: OnceLock<AgentTokenIssuer> = OnceLock::new();
        CLOUD.get_or_init(|| {
            AgentTokenIssuer::new(
                "https://cloud.example.com",
                Arc::new(RsaTokenSigner::generate("cloud-key-1").unwrap()),
            )
            .unwrap()
        })
    }

    async fn jwks_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(cloud().jwks()))
            .mount(&server)
            .await;
        server
    }

    fn validator(server: &MockServer, issuer: &str, audience: &str) -> Arc<dyn TokenValidator> {
        Arc::new(
            JwtValidator::builder()
                .issuer(issuer)
                .audience(audience)
                .jwks_uri(format!("{}/keys", server.uri()))
                .algorithm(jsonwebtoken::Algorithm::RS256)
                .build()
                .unwrap(),
        )
    }

    /// `claims` with a lifetime of ten minutes from now.
    async fn signed(mut claims: Value) -> String {
        let now = chrono::Utc::now().timestamp();
        claims["iat"] = json!(now);
        claims["nbf"] = json!(now);
        claims["exp"] = json!(now + 600);
        cloud().sign_claims(&claims).await.unwrap()
    }

    /// The claims Entra puts in an agent identity's v2.0 token for a custom audience.
    fn entra_custom_audience_claims() -> Value {
        json!({
            "iss": ENTRA_ISSUER,
            "aud": BLUEPRINT,
            "sub": AGENT,
            "oid": AGENT,
            "azp": AGENT,
            "azpacr": "2",
            "tid": TENANT,
            "ver": "2.0",
        })
    }

    fn google_subject() -> String {
        format!(
            "spiffe://{TRUST_DOMAIN}/resources/aiplatform/projects/987654321098/locations/us-central1/reasoningEngines/4242"
        )
    }

    fn google_issuer() -> String {
        format!(
            "https://sts.googleapis.com/v1/organizations/123456789012/locations/global/workloadIdentityPools/{TRUST_DOMAIN}"
        )
    }

    fn claims(value: Value) -> TokenClaims {
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn an_entra_token_for_a_custom_audience_names_the_agent_without_facets() {
        let server = jwks_server().await;
        let verifier = InboundAgentVerifier::new(
            validator(&server, ENTRA_ISSUER, BLUEPRINT),
            Arc::new(EntraAgentProfile),
        );

        let principal =
            verifier.verify(&signed(entra_custom_audience_claims()).await).await.unwrap();

        let expected =
            AgentPrincipal::new(AgentId::new(format!("urn:entra:{TENANT}:agent:{AGENT}")).unwrap())
                .with_tenant(TENANT)
                .with_attribute("entra.tenant_id", TENANT)
                .with_attribute("entra.agent_identity_id", AGENT)
                .with_attribute("entra.object_id", AGENT)
                .with_attribute("entra.token_version", "2.0");
        assert_eq!(principal, expected);
    }

    #[test]
    fn entra_agent_marking_claims_are_recorded_when_present() {
        let mut token = entra_custom_audience_claims();
        token["xms_par_app_azp"] = json!(BLUEPRINT);
        token["xms_act_fct"] = json!("3 11 9");
        token["xms_sub_fct"] = json!("11 3 9");
        token["idtyp"] = json!("app");

        let principal = EntraAgentProfile.principal(&claims(token)).unwrap();

        assert_eq!(principal.class.as_deref(), Some(BLUEPRINT));
        assert_eq!(principal.attributes["entra.blueprint_app_id"], BLUEPRINT);
        assert_eq!(principal.attributes["entra.identity_type"], "app");
        assert_eq!(principal.attributes["entra.actor_facets"], "3 11 9");
        assert_eq!(principal.attributes["entra.subject_facets"], "11 3 9");
        assert_eq!(principal.attributes["entra.agent_identity_facet"], "true");
    }

    #[test]
    fn an_entra_facet_list_without_11_is_not_an_agent_marker() {
        let mut token = entra_custom_audience_claims();
        token["xms_act_fct"] = json!("3 110 9");
        let principal = EntraAgentProfile.principal(&claims(token)).unwrap();
        assert!(!principal.attributes.contains_key("entra.agent_identity_facet"));
    }

    #[test]
    fn an_entra_v1_token_names_the_agent_by_appid() {
        let principal = EntraAgentProfile
            .principal(&claims(
                json!({ "sub": AGENT, "tid": TENANT, "appid": AGENT, "ver": "1.0" }),
            ))
            .unwrap();
        assert_eq!(principal.id.as_str(), format!("urn:entra:{TENANT}:agent:{AGENT}"));
        assert_eq!(principal.attributes["entra.token_version"], "1.0");
    }

    #[test]
    fn an_entra_token_without_azp_or_tid_is_refused() {
        let no_app = EntraAgentProfile.principal(&claims(json!({ "sub": AGENT, "tid": TENANT })));
        assert!(
            matches!(&no_app, Err(FederationError::Claim { claim, .. }) if claim == "azp"),
            "{no_app:?}"
        );
        let no_tenant = EntraAgentProfile.principal(&claims(json!({ "sub": AGENT, "azp": AGENT })));
        assert!(
            matches!(&no_tenant, Err(FederationError::Claim { claim, .. }) if claim == "tid"),
            "{no_tenant:?}"
        );
        let error: adk_core::AdkError = no_tenant.unwrap_err().into();
        assert_eq!(error.code, "identity.federation.claim_invalid");
    }

    #[tokio::test]
    async fn a_token_for_another_audience_or_with_a_bad_signature_is_refused() {
        let server = jwks_server().await;
        let verifier = InboundAgentVerifier::new(
            validator(&server, ENTRA_ISSUER, BLUEPRINT),
            Arc::new(EntraAgentProfile),
        );

        let mut other_audience = entra_custom_audience_claims();
        other_audience["aud"] = json!("another-service");
        let refused = verifier.verify(&signed(other_audience).await).await;
        assert!(
            matches!(&refused, Err(FederationError::Token(TokenError::ValidationError(_)))),
            "{refused:?}"
        );

        let token = signed(entra_custom_audience_claims()).await;
        let (signing_input, _) = token.rsplit_once('.').unwrap();
        let other = AgentTokenIssuer::new(
            "https://cloud.example.com",
            Arc::new(RsaTokenSigner::generate("cloud-key-1").unwrap()),
        )
        .unwrap();
        let forged = other.sign_claims(&entra_custom_audience_claims()).await.unwrap();
        let tampered = format!("{signing_input}.{}", forged.rsplit_once('.').unwrap().1);
        let refused = verifier.verify(&tampered).await;
        assert!(
            matches!(&refused, Err(FederationError::Token(TokenError::InvalidSignature))),
            "{refused:?}"
        );
        let error: adk_core::AdkError = refused.unwrap_err().into();
        assert_eq!(error.code, "auth.invalid_signature");
    }

    #[tokio::test]
    async fn a_google_agent_token_names_the_agent_by_its_spiffe_id() {
        let server = jwks_server().await;
        let audience = "https://shop.example.com";
        let verifier = InboundAgentVerifier::new(
            validator(&server, &google_issuer(), audience),
            Arc::new(GoogleAgentProfile),
        );
        let token = signed(json!({
            "iss": google_issuer(),
            "sub": google_subject(),
            "aud": [audience],
            "google": { "opaque": true },
        }))
        .await;

        let principal = verifier.verify(&token).await.unwrap();

        let expected = AgentPrincipal::new(AgentId::new(google_subject()).unwrap())
            .with_tenant(TRUST_DOMAIN)
            .with_attribute("google.service", "aiplatform")
            .with_attribute("google.project_number", "987654321098")
            .with_attribute("google.location", "us-central1")
            .with_attribute("google.resource_type", "reasoningEngines")
            .with_attribute("google.resource_id", "4242")
            .with_attribute("google.bound_token", "false");
        assert_eq!(principal, expected);
    }

    #[test]
    fn a_bound_google_token_records_its_certificate_thumbprint() {
        let principal = GoogleAgentProfile
            .principal(&claims(json!({
                "iss": google_issuer(),
                "sub": google_subject(),
                "cnf": { "x5t#S256": "bwcK0esc3ACC3DB2Y5_lESsXE8o9ltc05O89jdN-dg2" },
            })))
            .unwrap();
        assert_eq!(principal.attributes["google.bound_token"], "true");
        assert_eq!(
            principal.attributes["google.cnf_x5t_s256"],
            "bwcK0esc3ACC3DB2Y5_lESsXE8o9ltc05O89jdN-dg2"
        );
    }

    #[test]
    fn a_google_subject_of_another_shape_keeps_its_id_and_trust_domain() {
        let subject = format!("spiffe://{TRUST_DOMAIN}/ns/default/sa/agent");
        let principal = GoogleAgentProfile
            .principal(&claims(json!({ "iss": google_issuer(), "sub": subject })))
            .unwrap();
        let expected = AgentPrincipal::new(AgentId::new(subject).unwrap())
            .with_tenant(TRUST_DOMAIN)
            .with_attribute("google.bound_token", "false");
        assert_eq!(principal, expected);
    }

    #[test]
    fn a_google_subject_outside_the_issuing_pool_is_refused() {
        let foreign = "spiffe://agents.global.org-999.system.id.goog/resources/aiplatform/projects/1/locations/us/reasoningEngines/1";
        let refused = GoogleAgentProfile
            .principal(&claims(json!({ "iss": google_issuer(), "sub": foreign })));
        assert!(
            matches!(&refused, Err(FederationError::Claim { claim, .. }) if claim == "sub"),
            "{refused:?}"
        );
        let not_spiffe = GoogleAgentProfile
            .principal(&claims(json!({ "iss": google_issuer(), "sub": "1234567890" })));
        assert!(matches!(&not_spiffe, Err(FederationError::Claim { .. })), "{not_spiffe:?}");
    }

    #[test]
    fn the_entra_verifier_needs_one_tenant_and_an_audience() {
        for tenant in ["", "common", "organizations", "a/b"] {
            let refused = InboundAgentVerifier::entra(tenant, BLUEPRINT);
            assert!(
                matches!(&refused, Err(FederationError::InvalidConfig { .. })),
                "{tenant}: {refused:?}"
            );
        }
        let no_audience = InboundAgentVerifier::entra(TENANT, "");
        assert!(
            matches!(&no_audience, Err(FederationError::InvalidConfig { .. })),
            "{no_audience:?}"
        );
        let verifier = InboundAgentVerifier::entra(TENANT, BLUEPRINT).unwrap();
        assert!(format!("{verifier:?}").contains(ENTRA_ISSUER));
    }
}
