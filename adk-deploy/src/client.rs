use std::{fs, path::Path};

use reqwest::multipart::{Form, Part};
use reqwest::{Client, Method};
use serde::de::DeserializeOwned;
use url::Url;

use crate::{
    AgentDetail, AuthSessionResponse, DashboardResponse, DeployClientConfig, DeployError,
    DeployResult, DeploymentHistoryResponse, DeploymentStatusResponse, LoginRequest, LoginResponse,
    PushDeploymentRequest, PushDeploymentResponse, SecretListResponse, SecretSetRequest,
};

pub struct DeployClient {
    http: Client,
    config: DeployClientConfig,
}

impl DeployClient {
    pub fn new(config: DeployClientConfig) -> Self {
        Self { http: Client::new(), config }
    }

    pub fn config(&self) -> &DeployClientConfig {
        &self.config
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.config.token = Some(token.into());
        self
    }

    pub async fn login(&mut self, request: &LoginRequest) -> DeployResult<LoginResponse> {
        self.login_with_options(request, true).await
    }

    pub async fn login_ephemeral(&mut self, request: &LoginRequest) -> DeployResult<LoginResponse> {
        self.login_with_options(request, false).await
    }

    async fn login_with_options(
        &mut self,
        request: &LoginRequest,
        persist: bool,
    ) -> DeployResult<LoginResponse> {
        let response: LoginResponse =
            self.request(Method::POST, "/api/v1/auth/login", Some(request)).await?;
        self.config.token = Some(response.token.clone());
        self.config.workspace_id = Some(response.workspace_id.clone());
        if persist {
            self.config.save()?;
        }
        Ok(response)
    }

    pub async fn push_deployment(
        &self,
        request: &PushDeploymentRequest,
    ) -> DeployResult<PushDeploymentResponse> {
        let bundle_bytes = fs::read(&request.bundle_path)?;
        let request_json = serde_json::to_string(request)
            .map_err(|error| DeployError::Client { message: error.to_string() })?;
        let file_name = Path::new(&request.bundle_path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("bundle.tar.gz")
            .to_string();
        let form = Form::new()
            .part(
                "request",
                Part::text(request_json)
                    .mime_str("application/json")
                    .map_err(|error| DeployError::Client { message: error.to_string() })?,
            )
            .part(
                "bundle",
                Part::bytes(bundle_bytes)
                    .file_name(file_name)
                    .mime_str("application/gzip")
                    .map_err(|error| DeployError::Client { message: error.to_string() })?,
            );
        self.multipart_request(Method::POST, "/api/v1/deployments", form).await
    }

    pub async fn dashboard(&self) -> DeployResult<DashboardResponse> {
        self.request::<(), DashboardResponse>(Method::GET, "/api/v1/dashboard", None).await
    }

    pub async fn auth_session(&self) -> DeployResult<AuthSessionResponse> {
        self.request::<(), AuthSessionResponse>(Method::GET, "/api/v1/auth/session", None).await
    }

    pub async fn agent_detail(
        &self,
        agent_name: &str,
        environment: &str,
    ) -> DeployResult<AgentDetail> {
        let path = build_url_with_query(
            &format!("/api/v1/agents/{}", encode_path_segment(agent_name)),
            &[("environment", environment)],
        )?;
        self.request::<(), AgentDetail>(Method::GET, &path, None).await
    }

    pub async fn status(
        &self,
        environment: &str,
        agent_name: Option<&str>,
    ) -> DeployResult<DeploymentStatusResponse> {
        let mut query = vec![("environment", environment)];
        if let Some(agent_name) = agent_name {
            query.push(("agent", agent_name));
        }
        let path = build_url_with_query("/api/v1/deployments/status", &query)?;
        self.request::<(), DeploymentStatusResponse>(Method::GET, &path, None).await
    }

    pub async fn history(
        &self,
        environment: &str,
        agent_name: Option<&str>,
    ) -> DeployResult<DeploymentHistoryResponse> {
        let mut query = vec![("environment", environment)];
        if let Some(agent_name) = agent_name {
            query.push(("agent", agent_name));
        }
        let path = build_url_with_query("/api/v1/deployments/history", &query)?;
        self.request::<(), DeploymentHistoryResponse>(Method::GET, &path, None).await
    }

    pub async fn rollback(&self, deployment_id: &str) -> DeployResult<DeploymentStatusResponse> {
        let path = format!("/api/v1/deployments/{}/rollback", encode_path_segment(deployment_id));
        self.request::<(), DeploymentStatusResponse>(Method::POST, &path, None).await
    }

    pub async fn promote(&self, deployment_id: &str) -> DeployResult<DeploymentStatusResponse> {
        let path = format!("/api/v1/deployments/{}/promote", encode_path_segment(deployment_id));
        self.request::<(), DeploymentStatusResponse>(Method::POST, &path, None).await
    }

    pub async fn set_secret(&self, request: &SecretSetRequest) -> DeployResult<()> {
        let _: serde_json::Value =
            self.request(Method::POST, "/api/v1/secrets", Some(request)).await?;
        Ok(())
    }

    pub async fn list_secrets(&self, environment: &str) -> DeployResult<SecretListResponse> {
        let path = build_url_with_query("/api/v1/secrets", &[("environment", environment)])?;
        self.request::<(), SecretListResponse>(Method::GET, &path, None).await
    }

    pub async fn delete_secret(&self, environment: &str, key: &str) -> DeployResult<()> {
        let path = build_url_with_query(
            &format!("/api/v1/secrets/{}", encode_path_segment(key)),
            &[("environment", environment)],
        )?;
        let _: serde_json::Value =
            self.request::<(), serde_json::Value>(Method::DELETE, &path, None).await?;
        Ok(())
    }

    async fn request<T, R>(&self, method: Method, path: &str, body: Option<&T>) -> DeployResult<R>
    where
        T: serde::Serialize,
        R: DeserializeOwned,
    {
        let url = format!("{}{}", self.config.endpoint.trim_end_matches('/'), path);
        let mut request = self.http.request(method, url);
        if let Some(token) = &self.config.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let detail = body.trim();
            return Err(DeployError::Client {
                message: if detail.is_empty() {
                    format!("request failed with status {status}")
                } else {
                    format!("request failed with status {status}: {detail}")
                },
            });
        }
        response
            .json::<R>()
            .await
            .map_err(|error| DeployError::Client { message: error.to_string() })
    }

    async fn multipart_request<R>(&self, method: Method, path: &str, form: Form) -> DeployResult<R>
    where
        R: DeserializeOwned,
    {
        let url = format!("{}{}", self.config.endpoint.trim_end_matches('/'), path);
        let mut request = self.http.request(method, url);
        if let Some(token) = &self.config.token {
            request = request.bearer_auth(token);
        }
        let response = request.multipart(form).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let detail = body.trim();
            return Err(DeployError::Client {
                message: if detail.is_empty() {
                    format!("request failed with status {status}")
                } else {
                    format!("request failed with status {status}: {detail}")
                },
            });
        }
        response
            .json::<R>()
            .await
            .map_err(|error| DeployError::Client { message: error.to_string() })
    }
}

fn build_url_with_query(path: &str, query: &[(&str, &str)]) -> DeployResult<String> {
    let mut url = Url::parse(&format!("https://local{path}"))
        .map_err(|error| DeployError::Client { message: error.to_string() })?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let mut value = url.path().to_string();
    if let Some(query) = url.query() {
        value.push('?');
        value.push_str(query);
    }
    Ok(value)
}

fn encode_path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::{build_url_with_query, encode_path_segment};

    #[test]
    fn build_url_with_query_encodes_reserved_characters() {
        let path = build_url_with_query(
            "/api/v1/deployments/status",
            &[("environment", "prod blue"), ("agent", "agent/one")],
        )
        .unwrap();

        assert_eq!(path, "/api/v1/deployments/status?environment=prod+blue&agent=agent%2Fone");
    }

    #[test]
    fn encode_path_segment_escapes_slashes_and_spaces() {
        assert_eq!(encode_path_segment("prod/agent name"), "prod%2Fagent+name");
    }
}
