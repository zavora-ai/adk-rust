use std::{env, sync::Arc};

use adk_auth::sso::{JwtValidator, TokenClaims, TokenValidator};

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: String,
    pub workspace_id: String,
    pub scopes: Vec<String>,
}

#[derive(Clone)]
pub struct AuthService {
    backend: Option<Arc<AuthBackend>>,
    workspace_claim: Arc<str>,
    dev_login_enabled: bool,
}

#[derive(Clone)]
enum AuthBackend {
    Static(StaticTokenAuth),
    Jwt(Arc<JwtValidator>),
}

#[derive(Clone)]
struct StaticTokenAuth {
    token: String,
    user_id: String,
    workspace_id: Option<String>,
    scopes: Vec<String>,
}

impl AuthService {
    pub fn from_env() -> Result<Self, String> {
        let dev_login_enabled = env_flag("ADK_DEPLOY_ENABLE_DEV_LOGIN");
        let workspace_claim = Arc::<str>::from(
            env::var("ADK_DEPLOY_WORKSPACE_CLAIM")
                .unwrap_or_else(|_| "adk_workspace_id".to_string()),
        );

        if let Ok(token) = env::var("ADK_DEPLOY_BOOTSTRAP_TOKEN") {
            return Ok(Self {
                backend: Some(Arc::new(AuthBackend::Static(StaticTokenAuth {
                    token,
                    user_id: env::var("ADK_DEPLOY_BOOTSTRAP_USER_ID")
                        .unwrap_or_else(|_| "platform-admin".to_string()),
                    workspace_id: env::var("ADK_DEPLOY_BOOTSTRAP_WORKSPACE_ID").ok(),
                    scopes: env::var("ADK_DEPLOY_BOOTSTRAP_SCOPES")
                        .unwrap_or_else(|_| "deploy:read deploy:write deploy:admin".to_string())
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                }))),
                workspace_claim,
                dev_login_enabled,
            });
        }

        if let (Ok(issuer), Ok(jwks_uri)) =
            (env::var("ADK_DEPLOY_JWT_ISSUER"), env::var("ADK_DEPLOY_JWKS_URI"))
        {
            let mut builder = JwtValidator::builder().issuer(issuer).jwks_uri(jwks_uri);
            if let Ok(audience) = env::var("ADK_DEPLOY_JWT_AUDIENCE") {
                builder = builder.audience(audience);
            }
            let validator = builder.build().map_err(|error| error.to_string())?;
            return Ok(Self {
                backend: Some(Arc::new(AuthBackend::Jwt(Arc::new(validator)))),
                workspace_claim,
                dev_login_enabled,
            });
        }

        if dev_login_enabled {
            return Ok(Self { backend: None, workspace_claim, dev_login_enabled });
        }

        Err(
            "configure ADK_DEPLOY_BOOTSTRAP_TOKEN or ADK_DEPLOY_JWT_ISSUER/ADK_DEPLOY_JWKS_URI for control-plane authentication"
                .to_string(),
        )
    }

    #[cfg(test)]
    pub fn test_default() -> Self {
        Self {
            backend: Some(Arc::new(AuthBackend::Static(StaticTokenAuth {
                token: "test-token".to_string(),
                user_id: "test-operator".to_string(),
                workspace_id: None,
                scopes: vec![
                    "deploy:read".to_string(),
                    "deploy:write".to_string(),
                    "deploy:admin".to_string(),
                ],
            }))),
            workspace_claim: Arc::from("adk_workspace_id"),
            dev_login_enabled: true,
        }
    }

    pub fn dev_login_enabled(&self) -> bool {
        self.dev_login_enabled
    }

    pub async fn authenticate(
        &self,
        token: &str,
        default_workspace_id: &str,
    ) -> Result<Option<AuthContext>, String> {
        let Some(backend) = &self.backend else {
            return Ok(None);
        };
        let context = match backend.as_ref() {
            AuthBackend::Static(static_auth) => {
                static_auth.authenticate(token, default_workspace_id)
            }
            AuthBackend::Jwt(validator) => {
                let claims = validator.validate(token).await.map_err(|error| error.to_string())?;
                auth_context_from_claims(&claims, &self.workspace_claim, default_workspace_id)
            }
        }?;
        Ok(Some(context))
    }
}

impl StaticTokenAuth {
    fn authenticate(&self, token: &str, default_workspace_id: &str) -> Result<AuthContext, String> {
        if token != self.token {
            return Err("invalid bootstrap token".to_string());
        }
        Ok(AuthContext {
            user_id: self.user_id.clone(),
            workspace_id: self
                .workspace_id
                .clone()
                .unwrap_or_else(|| default_workspace_id.to_string()),
            scopes: self.scopes.clone(),
        })
    }
}

fn auth_context_from_claims(
    claims: &TokenClaims,
    workspace_claim: &str,
    default_workspace_id: &str,
) -> Result<AuthContext, String> {
    let workspace_id = claims
        .get_custom::<String>(workspace_claim)
        .unwrap_or_else(|| default_workspace_id.to_string());
    if workspace_id.trim().is_empty() {
        return Err("resolved empty workspace id from token claims".to_string());
    }
    Ok(AuthContext { user_id: claims.user_id().to_string(), workspace_id, scopes: claims.scopes() })
}

fn env_flag(key: &str) -> bool {
    env::var(key)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}
