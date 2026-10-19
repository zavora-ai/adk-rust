//! Agent CRUD methods for [`EnterpriseClient`].
//!
//! Implements Requirements 2.1–2.7:
//! - `create_agent` — POST /agents (with Idempotency-Key header)
//! - `get_agent` — GET /agents/{id}
//! - `list_agents` — GET /agents (cursor pagination)
//! - `update_agent` — PATCH /agents/{id}
//! - `archive_agent` — POST /agents/{id}/archive
//! - `delete_agent` — DELETE /agents/{id}

use crate::Result;
use crate::client::EnterpriseClient;
use crate::idempotency::IDEMPOTENCY_KEY_HEADER;
use crate::response::{handle_empty_response, handle_response};
use crate::retry::{RetryPolicy, execute_create_with_retry, execute_with_retry};
use crate::types::agent::{Agent, CreateAgentParams, UpdateAgentParams};
use crate::types::pagination::{ListParams, ListResponse};

impl EnterpriseClient {
    /// Create a new agent configuration.
    ///
    /// POSTs to `/agents` with an `Idempotency-Key` header to ensure replay safety.
    /// Returns the server-assigned agent with `id`, `version`, and timestamps.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_enterprise::{EnterpriseClient, CreateAgentParams};
    ///
    /// let client = EnterpriseClient::new("adk_live_...")?;
    /// let agent = client.create_agent(CreateAgentParams {
    ///     name: "My Agent".into(),
    ///     model: "gemini-3.7-flash".into(),
    ///     ..Default::default()
    /// }).await?;
    /// println!("Created agent: {}", agent.id);
    /// ```
    pub async fn create_agent(&self, params: CreateAgentParams) -> Result<Agent> {
        let url = self.build_url("/agents");
        let headers = self.default_headers();
        let policy = RetryPolicy::from_config(self.config.max_retries, self.config.retry_backoff);

        let body = serde_json::to_vec(&params)?;

        let response = execute_create_with_retry(&policy, |idempotency_key| {
            let url = url.clone();
            let headers = headers.clone();
            let body = body.clone();
            async move {
                reqwest::Client::new()
                    .post(&url)
                    .headers(headers)
                    .header(IDEMPOTENCY_KEY_HEADER, idempotency_key)
                    .body(body)
                    .send()
                    .await
            }
        })
        .await?;

        handle_response(response).await
    }

    /// Retrieve an agent by ID.
    ///
    /// GETs `/agents/{id}`. Returns `EnterpriseError::NotFound` if the agent
    /// does not exist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = client.get_agent("agt_abc123").await?;
    /// println!("Agent name: {}", agent.name);
    /// ```
    pub async fn get_agent(&self, agent_id: &str) -> Result<Agent> {
        let url = self.build_url(&format!("/agents/{agent_id}"));
        let headers = self.default_headers();
        let policy = RetryPolicy::from_config(self.config.max_retries, self.config.retry_backoff);

        let response = execute_with_retry(&policy, || {
            let url = url.clone();
            let headers = headers.clone();
            async move { reqwest::Client::new().get(&url).headers(headers).send().await }
        })
        .await?;

        handle_response(response).await
    }

    /// List agents with optional cursor pagination.
    ///
    /// GETs `/agents` with optional `limit` and `cursor` query parameters.
    /// Returns a paginated `ListResponse<Agent>`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_enterprise::ListParams;
    ///
    /// let response = client.list_agents(Some(ListParams {
    ///     limit: Some(10),
    ///     cursor: None,
    /// })).await?;
    ///
    /// for agent in &response.data {
    ///     println!("  {} ({})", agent.name, agent.id);
    /// }
    ///
    /// if response.has_more {
    ///     // Use response.next_cursor for next page
    /// }
    /// ```
    pub async fn list_agents(&self, params: Option<ListParams>) -> Result<ListResponse<Agent>> {
        let url = self.build_url("/agents");
        let headers = self.default_headers();
        let policy = RetryPolicy::from_config(self.config.max_retries, self.config.retry_backoff);

        let query_params = build_list_query_params(params.as_ref());

        let response = execute_with_retry(&policy, || {
            let url = url.clone();
            let headers = headers.clone();
            let query_params = query_params.clone();
            async move {
                reqwest::Client::new().get(&url).headers(headers).query(&query_params).send().await
            }
        })
        .await?;

        handle_response(response).await
    }

    /// Update an existing agent configuration.
    ///
    /// PATCHes `/agents/{id}` with the provided update parameters. Only fields
    /// set in `UpdateAgentParams` are modified. Returns the updated agent with
    /// a bumped `version`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_enterprise::UpdateAgentParams;
    ///
    /// let updated = client.update_agent("agt_abc123", UpdateAgentParams {
    ///     name: Some("Renamed Agent".into()),
    ///     ..Default::default()
    /// }).await?;
    /// println!("New version: {}", updated.version);
    /// ```
    pub async fn update_agent(&self, agent_id: &str, params: UpdateAgentParams) -> Result<Agent> {
        let url = self.build_url(&format!("/agents/{agent_id}"));
        let headers = self.default_headers();
        let policy = RetryPolicy::from_config(self.config.max_retries, self.config.retry_backoff);

        let body = serde_json::to_vec(&params)?;

        let response =
            execute_with_retry(&policy, || {
                let url = url.clone();
                let headers = headers.clone();
                let body = body.clone();
                async move {
                    reqwest::Client::new().patch(&url).headers(headers).body(body).send().await
                }
            })
            .await?;

        handle_response(response).await
    }

    /// Archive an agent.
    ///
    /// POSTs to `/agents/{id}/archive`. The agent is marked as archived and can
    /// no longer be used to create new sessions, but existing sessions continue.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let archived = client.archive_agent("agt_abc123").await?;
    /// assert!(archived.archived_at.is_some());
    /// ```
    pub async fn archive_agent(&self, agent_id: &str) -> Result<Agent> {
        let url = self.build_url(&format!("/agents/{agent_id}/archive"));
        let headers = self.default_headers();
        let policy = RetryPolicy::from_config(self.config.max_retries, self.config.retry_backoff);

        let response = execute_with_retry(&policy, || {
            let url = url.clone();
            let headers = headers.clone();
            async move { reqwest::Client::new().post(&url).headers(headers).send().await }
        })
        .await?;

        handle_response(response).await
    }

    /// Permanently delete an agent.
    ///
    /// DELETEs `/agents/{id}`. This is irreversible. Returns `Ok(())` on success,
    /// or `EnterpriseError::NotFound` if the agent does not exist.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// client.delete_agent("agt_abc123").await?;
    /// ```
    pub async fn delete_agent(&self, agent_id: &str) -> Result<()> {
        let url = self.build_url(&format!("/agents/{agent_id}"));
        let headers = self.default_headers();
        let policy = RetryPolicy::from_config(self.config.max_retries, self.config.retry_backoff);

        let response = execute_with_retry(&policy, || {
            let url = url.clone();
            let headers = headers.clone();
            async move { reqwest::Client::new().delete(&url).headers(headers).send().await }
        })
        .await?;

        handle_empty_response(response).await
    }
}

/// Build query parameters for list endpoints from optional `ListParams`.
fn build_list_query_params(params: Option<&ListParams>) -> Vec<(String, String)> {
    let mut query = Vec::new();
    if let Some(p) = params {
        if let Some(limit) = p.limit {
            query.push(("limit".to_string(), limit.to_string()));
        }
        if let Some(ref cursor) = p.cursor {
            query.push(("cursor".to_string(), cursor.clone()));
        }
    }
    query
}
