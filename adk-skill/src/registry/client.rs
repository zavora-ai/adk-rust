//! REST client for the Vertex AI Skill Registry v1beta1 (read-only).

use crate::error::SkillError;
use crate::registry::extract::{extract_skill_archive, write_files_to_dir};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, truncate_for_error};
use base64::Engine as _;
use google_cloud_auth::credentials::Credentials;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SKILL_REGISTRY_API_VERSION: &str = "v1beta1";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const AUTH_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";
/// Server-side default and maximum for `skills:retrieve` `topK`.
const RETRIEVE_MAX_TOP_K: u32 = 100;

/// Configuration for the Vertex AI Skill Registry client.
///
/// > **Note:** the Skill Registry API is **v1beta1 (Preview)**, served from
/// > regional `https://{location}-aiplatform.googleapis.com` endpoints in
/// > `us-central1`, `europe-west4`, and `us-east5` only. It is a Build-pillar
/// > service distinct from the Agent Registry (a separate Govern-pillar
/// > service).
#[derive(Debug, Clone)]
pub struct SkillRegistryConfig {
    /// Google Cloud project ID.
    pub project_id: String,
    /// GCP region (`us-central1`, `europe-west4`, or `us-east5`).
    pub location: String,
    /// Optional custom API origin.
    ///
    /// The origin receives Google authorization headers plus skill payloads.
    /// It must not contain userinfo, a path, a query, or a fragment.
    pub endpoint: Option<String>,
}

impl SkillRegistryConfig {
    /// Creates a new config with the given project ID and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self { project_id: project_id.into(), location: location.into(), endpoint: None }
    }

    /// Builds a config from environment variables.
    ///
    /// Reads `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`. Values are
    /// trimmed; blank values count as missing.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_skill::registry::{SkillRegistryClient, SkillRegistryConfig};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = SkillRegistryConfig::from_env()?;
    /// let client = SkillRegistryClient::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error naming every missing or blank variable.
    pub fn from_env() -> Result<Self> {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let project_id = read(ENV_GOOGLE_CLOUD_PROJECT);
        let location = read(ENV_GOOGLE_CLOUD_LOCATION);

        match (project_id, location) {
            (Some(project_id), Some(location)) => Ok(Self::new(project_id, location)),
            (project_id, location) => {
                let missing = [
                    (ENV_GOOGLE_CLOUD_PROJECT, project_id.is_none()),
                    (ENV_GOOGLE_CLOUD_LOCATION, location.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, is_missing)| is_missing.then_some(key))
                .collect::<Vec<_>>()
                .join(", ");
                Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::InvalidInput,
                    "skill.registry.missing_env",
                    format!(
                        "missing or blank environment variable(s): {missing}. Set them, or construct the config with SkillRegistryConfig::new",
                    ),
                )
                .with_provider("vertex_ai"))
            }
        }
    }

    /// Sets a custom API origin.
    ///
    /// Use only a trusted HTTPS origin, or loopback HTTP for local tests.
    /// Userinfo, paths, queries, and fragments are rejected before transport.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    fn endpoint(&self) -> String {
        let endpoint = self
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}-aiplatform.googleapis.com", self.location));
        if endpoint.contains("://") { endpoint } else { format!("https://{endpoint}") }
    }

    fn parent_path(&self) -> String {
        format!("projects/{}/locations/{}", self.project_id, self.location)
    }
}

/// Lifecycle state of a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SkillState {
    /// State not specified.
    #[default]
    StateUnspecified,
    /// The skill is ready for use.
    Active,
    /// The skill is being created.
    Creating,
    /// Skill creation failed.
    Failed,
    /// The skill is being deleted.
    Deleting,
    /// A state this client version does not know about.
    #[serde(other)]
    Unknown,
}

/// Origin of a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SkillSource {
    /// Uploaded by a user.
    #[default]
    User,
    /// Provisioned by the platform — e.g. the built-in `gcp-skill-registry`
    /// skill, which the service provisions lazily on the first API call to a
    /// project/location.
    System,
    /// A source this client version does not know about.
    #[serde(other)]
    Unknown,
}

/// A skill resource as served by the registry.
///
/// `displayName` aligns with the `SKILL.md` frontmatter `name`; `description`,
/// `license`, and `compatibility` mirror their frontmatter counterparts. All
/// other frontmatter (e.g. `metadata.category`, `allowed-tools`) lives only
/// inside the zip payload.
///
/// Deserialization is lenient: every field is defaulted so undocumented or
/// elided fields never fail a response parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Skill {
    /// Full resource name: `projects/{p}/locations/{l}/skills/{skill}`.
    pub name: String,
    /// Human-readable name; aligns with the `SKILL.md` frontmatter `name`.
    pub display_name: String,
    /// What the skill does and when an agent should use it.
    pub description: String,
    /// Optional SPDX license identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Optional environment requirements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    /// Base64-encoded zip archive with `SKILL.md` at its root.
    ///
    /// Populated by `skills.get` (the latest revision's payload — this **is**
    /// the content download; there is no `:download` endpoint). May be elided
    /// on `skills.list` responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zipped_filesystem: Option<String>,
    /// Lifecycle state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SkillState>,
    /// User-defined labels.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Output only: SHA-256 hex digest of the decoded zip payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Whether the skill was uploaded by a user or provisioned by the system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_source: Option<SkillSource>,
    /// Output only: creation timestamp (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// Output only: last update timestamp (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
}

/// An immutable snapshot of a skill at a point in time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SkillRevision {
    /// Full resource name:
    /// `projects/{p}/locations/{l}/skills/{skill}/revisions/{revision}`.
    pub name: String,
    /// Output only: creation timestamp (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// The embedded full skill snapshot.
    ///
    /// Whether the snapshot's `zippedFilesystem` is populated is
    /// undocumented; treat it as optional and fall back to
    /// [`SkillRegistryClient::fetch_skill_content`] on the parent skill for
    /// the latest payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<Skill>,
    /// Lifecycle state of the revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SkillState>,
}

/// Response page from `skills.list`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListSkillsResponse {
    /// Skills on this page. `zippedFilesystem` may be elided.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Skill>,
    /// Token for the next page, when more results exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// Response page from `skills.revisions.list`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListSkillRevisionsResponse {
    /// Revisions on this page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skill_revisions: Vec<SkillRevision>,
    /// Token for the next page, when more results exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// One semantic search hit from `skills:retrieve`.
///
/// The API returns no scores; results are ranked by array order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RetrievedSkill {
    /// Full resource name of the matched skill.
    pub skill_name: String,
    /// Description of the matched skill.
    pub description: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RetrieveSkillsResponse {
    retrieved_skills: Vec<RetrievedSkill>,
}

/// A skill's verified, safely extracted zip payload.
///
/// Produced by [`SkillRegistryClient::fetch_skill_content`]. The `SKILL.md`
/// bytes surface via [`skill_md`](Self::skill_md) and can be fed directly to
/// [`parse_skill_markdown`](crate::parse_skill_markdown).
#[derive(Debug, Clone)]
pub struct SkillContent {
    /// Skill metadata as returned by `skills.get`, with `zippedFilesystem`
    /// cleared (the decoded contents live in [`files`](Self::files)).
    pub skill: Skill,
    /// SHA-256 hex digest computed over the decoded zip payload.
    pub sha256: String,
    /// Extracted files keyed by archive path (`SKILL.md` at the root).
    pub files: BTreeMap<String, Vec<u8>>,
}

impl SkillContent {
    /// Archive path of the skill definition file.
    pub const SKILL_MD: &'static str = "SKILL.md";

    /// The raw `SKILL.md` bytes, when present at the archive root.
    pub fn skill_md(&self) -> Option<&[u8]> {
        self.files.get(Self::SKILL_MD).map(Vec::as_slice)
    }

    /// Writes the extracted files under `dir`, returning the written paths.
    ///
    /// # Errors
    ///
    /// Returns [`SkillError::Io`] when a file or directory cannot be written.
    pub fn write_to_dir(&self, dir: &Path) -> std::result::Result<Vec<PathBuf>, SkillError> {
        write_files_to_dir(&self.files, dir)
    }
}

const GCP_ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "skill.registry.invalid_input",
    unauthorized: "skill.registry.unauthorized",
    forbidden: "skill.registry.forbidden",
    not_found: "skill.registry.not_found",
    rate_limited: "skill.registry.rate_limited",
    timeout: "skill.registry.timeout",
    unavailable: "skill.registry.unavailable",
    credentials_unavailable: "skill.registry.credentials_unavailable",
    invalid_response: "skill.registry.invalid_response",
    invalid_request: "skill.registry.invalid_request",
    upstream_error: "skill.registry.upstream_error",
    // The read-only surface has no long-running operations; required by the
    // table but never stamped.
    operation_failed: "skill.registry.operation_failed",
};

/// ADC-authenticated, read-only REST client for the Skill Registry v1beta1.
///
/// Performs [`get_skill`](Self::get_skill), [`list_skills`](Self::list_skills),
/// [`search_skills`](Self::search_skills),
/// [`list_skill_revisions`](Self::list_skill_revisions),
/// [`get_skill_revision`](Self::get_skill_revision), and
/// [`fetch_skill_content`](Self::fetch_skill_content) against
/// `projects/*/locations/*/skills/*` resources.
///
/// Lifecycle operations (create, update, delete) are intentionally
/// unimplemented — platform tooling owns them.
///
/// > **Note:** this is the **Skill Registry** (Build pillar, `SKILL.md`
/// > packages on `aiplatform.googleapis.com` v1beta1) — not the Agent
/// > Registry, a separate Govern-pillar service for cataloging agents.
///
/// > **Note:** the built-in `gcp-skill-registry` system skill is provisioned
/// > lazily on the first API call to a project/location, so a first `list`
/// > may return it even in an otherwise empty project.
pub struct SkillRegistryClient {
    client: GcpHttpClient,
    parent: String,
}

impl std::fmt::Debug for SkillRegistryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The transport carries credentials; expose only the parent resource.
        f.debug_struct("SkillRegistryClient").field("parent", &self.parent).finish_non_exhaustive()
    }
}

impl SkillRegistryClient {
    /// Creates a new client using Application Default Credentials (ADC).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_skill::registry::{SkillRegistryClient, SkillRegistryConfig};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = SkillRegistryConfig::new("my-project", "us-central1");
    /// let client = SkillRegistryClient::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not a
    /// valid secure origin, or the redirect-disabled HTTP client cannot be
    /// constructed.
    pub fn new_with_adc(config: SkillRegistryConfig) -> Result<Self> {
        Self::build(config, None)
    }

    /// Creates a new client with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin or the
    /// redirect-disabled HTTP client cannot be constructed.
    pub fn with_credentials(config: SkillRegistryConfig, credentials: Credentials) -> Result<Self> {
        Self::build(config, Some(credentials))
    }

    fn build(config: SkillRegistryConfig, credentials: Option<Credentials>) -> Result<Self> {
        let errors = GcpErrorContext::new(ErrorComponent::Tool, GCP_ERROR_CODES, "skill registry");
        let mut builder = GcpHttpClient::builder(errors, config.endpoint())
            .api_version(SKILL_REGISTRY_API_VERSION)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .request_timeout(HTTP_REQUEST_TIMEOUT)
            .auth_timeout(AUTH_HEADERS_TIMEOUT);
        if let Some(credentials) = credentials {
            builder = builder.credentials(credentials);
        }
        Ok(Self { client: builder.build()?, parent: config.parent_path() })
    }

    /// The `projects/*/locations/*` parent this client operates on.
    pub fn parent_resource_name(&self) -> &str {
        &self.parent
    }

    /// Resolves a bare skill ID or full resource name to a full name.
    fn skill_path(&self, skill: &str) -> String {
        if skill.contains('/') {
            skill.to_string()
        } else {
            format!("{}/skills/{skill}", self.parent)
        }
    }

    /// Gets a skill, including its `zippedFilesystem` payload.
    ///
    /// `GET {skill}`. This is also the content download: the response
    /// carries the latest revision's base64 zip payload; there is no
    /// separate `:download` endpoint. Prefer
    /// [`fetch_skill_content`](Self::fetch_skill_content) to get the payload
    /// decoded, digest-verified, and safely extracted.
    ///
    /// `skill` may be a bare skill ID or a full
    /// `projects/*/locations/*/skills/*` resource name.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn get_skill(&self, skill: &str) -> Result<Skill> {
        self.get_json(&self.skill_path(skill), &[]).await
    }

    /// Lists skills under the configured project/location.
    ///
    /// `GET {parent}/skills?pageSize=&pageToken=`. Listed skills may have
    /// `zippedFilesystem` elided; use [`get_skill`](Self::get_skill) or
    /// [`fetch_skill_content`](Self::fetch_skill_content) for the payload.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn list_skills(
        &self,
        page_size: Option<u32>,
        page_token: Option<&str>,
    ) -> Result<ListSkillsResponse> {
        let mut query = Vec::new();
        if let Some(page_size) = page_size {
            query.push(("pageSize", page_size.to_string()));
        }
        if let Some(page_token) = page_token {
            query.push(("pageToken", page_token.to_string()));
        }
        self.get_json(&format!("{}/skills", self.parent), &query).await
    }

    /// Semantically searches skills by `displayName` and `description`.
    ///
    /// `GET {parent}/skills:retrieve?query=&topK=` with an **empty request
    /// body**. `top_k` defaults to 10 server-side and caps at 100. The
    /// response carries no scores; results are ranked by array order.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when `top_k` exceeds 100, and
    /// otherwise an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn search_skills(
        &self,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<RetrievedSkill>> {
        if let Some(top_k) = top_k
            && top_k > RETRIEVE_MAX_TOP_K
        {
            return Err(self.client.errors().invalid_input(format!(
                "topK {top_k} exceeds the skills:retrieve maximum of {RETRIEVE_MAX_TOP_K}. Pass a value between 1 and {RETRIEVE_MAX_TOP_K}, or None for the server default of 10",
            )));
        }
        let mut params = vec![("query", query.to_string())];
        if let Some(top_k) = top_k {
            params.push(("topK", top_k.to_string()));
        }
        let response: RetrieveSkillsResponse =
            self.get_json(&format!("{}/skills:retrieve", self.parent), &params).await?;
        Ok(response.retrieved_skills)
    }

    /// Lists the revisions of a skill.
    ///
    /// `GET {skill}/revisions?pageSize=&pageToken=&filter=`. The only
    /// supported `filter` syntax is labels equality (e.g.
    /// `labels.env=prod`). There is no `revisions/latest` alias — the latest
    /// payload is what [`get_skill`](Self::get_skill) returns on the parent.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn list_skill_revisions(
        &self,
        skill: &str,
        page_size: Option<u32>,
        page_token: Option<&str>,
        filter: Option<&str>,
    ) -> Result<ListSkillRevisionsResponse> {
        let mut query = Vec::new();
        if let Some(page_size) = page_size {
            query.push(("pageSize", page_size.to_string()));
        }
        if let Some(page_token) = page_token {
            query.push(("pageToken", page_token.to_string()));
        }
        if let Some(filter) = filter {
            query.push(("filter", filter.to_string()));
        }
        self.get_json(&format!("{}/revisions", self.skill_path(skill)), &query).await
    }

    /// Gets a single revision of a skill.
    ///
    /// `GET {skill}/revisions/{revision}`. Whether the embedded skill
    /// snapshot carries `zippedFilesystem` is undocumented; see
    /// [`SkillRevision::skill`].
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn get_skill_revision(&self, skill: &str, revision: &str) -> Result<SkillRevision> {
        let path = if revision.contains('/') {
            revision.to_string()
        } else {
            format!("{}/revisions/{revision}", self.skill_path(skill))
        };
        self.get_json(&path, &[]).await
    }

    /// Downloads, verifies, and safely extracts a skill's content.
    ///
    /// Runs [`get_skill`](Self::get_skill), base64-decodes the
    /// `zippedFilesystem` payload, verifies its SHA-256 digest against the
    /// registry's `sha256` field, and extracts the archive in memory with
    /// the full defense-in-depth validation of
    /// [`extract_skill_archive`](crate::registry::extract_skill_archive).
    ///
    /// The returned [`SkillContent`] surfaces the raw `SKILL.md` bytes via
    /// [`SkillContent::skill_md`], ready for
    /// [`parse_skill_markdown`](crate::parse_skill_markdown).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_skill::registry::{SkillRegistryClient, SkillRegistryConfig};
    ///
    /// # async fn fetch() -> adk_core::Result<()> {
    /// let config = SkillRegistryConfig::new("my-project", "us-central1");
    /// let client = SkillRegistryClient::new_with_adc(config)?;
    /// let content = client.fetch_skill_content("my-skill").await?;
    /// let skill_md = content.skill_md().expect("SKILL.md at the archive root");
    /// println!("{} bytes of SKILL.md", skill_md.len());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, an unparseable response body, a missing or undecodable
    /// payload, a digest mismatch, or any archive-safety violation.
    pub async fn fetch_skill_content(&self, skill: &str) -> Result<SkillContent> {
        let mut skill = self.get_skill(skill).await?;
        let encoded = skill
            .zipped_filesystem
            .take()
            .filter(|payload| !payload.trim().is_empty())
            .ok_or_else(|| {
                self.client.errors().invalid_response(format!(
                    "skill `{}` response carried no zippedFilesystem payload; the skill may still be CREATING",
                    skill.name,
                ))
            })?;

        let bytes =
            base64::engine::general_purpose::STANDARD.decode(encoded.trim()).map_err(|error| {
                SkillError::RegistryPayloadDecode {
                    message: truncate_for_error(&error.to_string()),
                }
            })?;

        let computed = format!("{:x}", Sha256::digest(&bytes));
        match skill.sha256.as_deref().map(str::trim).filter(|digest| !digest.is_empty()) {
            Some(expected) if !expected.eq_ignore_ascii_case(&computed) => {
                return Err(SkillError::RegistryChecksumMismatch {
                    expected: expected.to_string(),
                    actual: computed,
                }
                .into());
            }
            Some(_) => {}
            None => {
                tracing::warn!(
                    skill.name = %skill.name,
                    "skill registry response carried no sha256 digest; skipping verification"
                );
            }
        }

        let files = extract_skill_archive(&bytes)?;
        tracing::debug!(
            skill.name = %skill.name,
            file.count = files.len(),
            "fetched and extracted skill content"
        );
        Ok(SkillContent { skill, sha256: computed, files })
    }

    async fn get_json<R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<R> {
        tracing::debug!(skill_registry.path = path, "sending skill registry request");
        let mut request = self.client.request(Method::GET, path).await?;
        if !query.is_empty() {
            request = request.query(query);
        }
        let value = self.client.send_value(request).await?;
        serde_json::from_value(value).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            self.client
                .errors()
                .invalid_response(format!("failed to parse skill registry response JSON: {error}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_config_resolves_paths_and_default_endpoint() {
        let config = SkillRegistryConfig::new("p", "europe-west4");
        assert_eq!(config.parent_path(), "projects/p/locations/europe-west4");
        assert_eq!(config.endpoint(), "https://europe-west4-aiplatform.googleapis.com");
    }

    #[test]
    fn test_skill_deserializes_leniently_with_unknown_state_and_fields() {
        let skill: Skill = serde_json::from_value(json!({
            "name": "projects/p/locations/l/skills/s",
            "displayName": "s",
            "description": "d",
            "state": "SOME_FUTURE_STATE",
            "skillSource": "SOME_FUTURE_SOURCE",
            "undocumentedField": { "nested": true },
        }))
        .unwrap();
        assert_eq!(skill.state, Some(SkillState::Unknown));
        assert_eq!(skill.skill_source, Some(SkillSource::Unknown));
        assert!(skill.zipped_filesystem.is_none());

        let revision: SkillRevision = serde_json::from_value(json!({
            "name": "projects/p/locations/l/skills/s/revisions/1",
            "updateTime": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        assert!(revision.skill.is_none());
    }

    // async: the credentials builder requires an ambient tokio runtime.
    #[tokio::test]
    async fn test_endpoint_rejects_cleartext_and_decorated_origins() {
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("k").build();
        let config =
            SkillRegistryConfig::new("p", "us-central1").with_endpoint("http://example.com");
        let error = SkillRegistryClient::with_credentials(config, credentials.clone()).unwrap_err();
        assert!(error.message.contains("HTTPS"), "unexpected error: {}", error.message);

        let config =
            SkillRegistryConfig::new("p", "us-central1").with_endpoint("https://example.com/path");
        let error = SkillRegistryClient::with_credentials(config, credentials).unwrap_err();
        assert!(error.message.contains("origin"), "unexpected error: {}", error.message);
    }
}
