//! REST client for the Agent Engine `sandboxEnvironments` surface.

use super::types::{
    Chunk, CreateSandboxRequest, ExecuteRequest, ExecuteResponse, InputFile, ListSandboxesResponse,
    SandboxEnvironment, SandboxExecutionResult, decode_output_chunks, encode_code_chunk,
    encode_file_chunk,
};
use super::{
    CREATE_POLL_TIMEOUT, HTTP_REQUEST_TIMEOUT, MAX_REQUEST_FILE_BYTES, MAX_RESPONSE_BYTES,
    VertexSandboxConfig, errors,
};
use adk_core::Result;
use adk_gcp::{
    GcpHttpClient, LroPoller, VertexResourceName, is_canonical_reasoning_engine_id,
    is_scoped_resource_name, truncate_for_error,
};
use google_cloud_auth::credentials::Credentials;
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;
use tracing::debug;

/// Maximum sandboxes per list page (service cap).
const LIST_PAGE_SIZE: usize = 100;
/// Upper bound on list pagination rounds, so a server that keeps returning
/// page tokens cannot spin this client forever.
const LIST_MAX_PAGES: usize = 1_000;

/// Client for Agent Engine sandbox environments.
///
/// Wraps the v1beta1 `sandboxEnvironments` REST surface: create and delete
/// are long-running operations (polled to completion), get and list are
/// plain reads, and `:execute` is synchronous. Resource names in every
/// follow-up request (operation polls, re-GETs, executes, deletes) are
/// validated against the configured project and location.
///
/// # Example
///
/// ```rust,no_run
/// use adk_code::vertex_sandbox::{
///     CreateSandboxRequest, VertexSandboxClient, VertexSandboxConfig,
/// };
///
/// # async fn run() -> adk_core::Result<()> {
/// let client = VertexSandboxClient::new_with_adc(VertexSandboxConfig::new(
///     "my-project",
///     "us-central1",
/// ))?;
/// let sandbox = client
///     .create_sandbox("4242", CreateSandboxRequest::new("my-sandbox").with_ttl("3600s"))
///     .await?;
/// # let _ = sandbox;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct VertexSandboxClient {
    http: GcpHttpClient,
    project_id: String,
    location: String,
    create_poller: LroPoller,
    delete_poller: LroPoller,
}

impl VertexSandboxClient {
    /// Creates a client using Application Default Credentials (ADC).
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not
    /// a valid secure origin, or the HTTP client cannot be built.
    pub fn new_with_adc(config: VertexSandboxConfig) -> Result<Self> {
        Self::build(config, None)
    }

    /// Creates a client with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin or
    /// the HTTP client cannot be built.
    pub fn with_credentials(config: VertexSandboxConfig, credentials: Credentials) -> Result<Self> {
        Self::build(config, Some(credentials))
    }

    fn build(config: VertexSandboxConfig, credentials: Option<Credentials>) -> Result<Self> {
        let mut builder = GcpHttpClient::builder(errors(), config.endpoint())
            .request_timeout(HTTP_REQUEST_TIMEOUT)
            .max_response_bytes(MAX_RESPONSE_BYTES);
        if let Some(credentials) = credentials {
            builder = builder.credentials(credentials);
        }
        Ok(Self {
            http: builder.build()?,
            project_id: config.project_id,
            location: config.location,
            create_poller: LroPoller::new()
                .with_poll_timeout(CREATE_POLL_TIMEOUT)
                .with_max_delay(Duration::from_secs(5)),
            delete_poller: LroPoller::new(),
        })
    }

    /// Creates a sandbox under the given reasoning engine and waits for the
    /// create operation to complete.
    ///
    /// `engine` is a full `projects/*/locations/*/reasoningEngines/*` name
    /// or a bare numeric engine ID resolved against the configured project
    /// and location. The operation response can be sparse, so the created
    /// sandbox is re-fetched by name before returning.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine reference or display name is
    /// invalid, the request fails, the operation completes with an error or
    /// times out, or the created sandbox cannot be fetched.
    pub async fn create_sandbox(
        &self,
        engine: &str,
        request: CreateSandboxRequest,
    ) -> Result<SandboxEnvironment> {
        let errors = self.http.errors();
        let engine = self.resolve_engine(engine)?;
        if request.display_name.trim().is_empty() {
            return Err(errors.invalid_input(
                "vertex sandbox display name must not be blank; pass a non-empty display name to CreateSandboxRequest::new",
            ));
        }
        let body = request.into_body();
        let http_request = self
            .http
            .request(Method::POST, &format!("{engine}/sandboxEnvironments"))
            .await?
            .json(&body);
        let operation = self.http.send_value(http_request).await?;
        let response = self
            .create_poller
            .wait_for_operation(
                &self.http,
                operation,
                "sandbox create",
                true,
                &self.project_id,
                &self.location,
            )
            .await?
            .unwrap_or(Value::Null);
        // The operation response carries the created sandbox but may be
        // sparse — read only the name, then re-GET the full resource.
        let name = response.get("name").and_then(Value::as_str).ok_or_else(|| {
            errors.invalid_response(
                "vertex sandbox create operation response did not contain a sandbox name; inspect the operation in Google Cloud",
            )
        })?;
        self.validate_sandbox_name(name)?;
        debug!(sandbox.name = name, "sandbox created, fetching full resource");
        self.get_sandbox(name).await
    }

    /// Fetches a sandbox by resource name.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is outside the configured scope, the
    /// request fails (a missing sandbox is a not-found error), or the
    /// response is not a sandbox resource.
    pub async fn get_sandbox(&self, name: &str) -> Result<SandboxEnvironment> {
        self.validate_sandbox_name(name)?;
        let request = self.http.request(Method::GET, name).await?;
        let value = self.http.send_value(request).await?;
        self.parse_sandbox(value)
    }

    /// Lists every sandbox under the given reasoning engine, following
    /// pagination (`pageSize` 100, the service cap).
    ///
    /// # Errors
    ///
    /// Returns an error when the engine reference is invalid, a page
    /// request fails, or the server keeps returning page tokens past the
    /// internal pagination bound.
    pub async fn list_sandboxes(&self, engine: &str) -> Result<Vec<SandboxEnvironment>> {
        let errors = self.http.errors();
        let engine = self.resolve_engine(engine)?;
        let path = format!("{engine}/sandboxEnvironments");
        let mut sandboxes = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..LIST_MAX_PAGES {
            let mut request = self
                .http
                .request(Method::GET, &path)
                .await?
                .query(&[("pageSize", LIST_PAGE_SIZE.to_string())]);
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token)]);
            }
            let value = self.http.send_value(request).await?;
            let page: ListSandboxesResponse = serde_json::from_value(value).map_err(|error| {
                errors.invalid_response(format!(
                    "failed to parse vertex sandbox list response: {}",
                    truncate_for_error(&error.to_string()),
                ))
            })?;
            sandboxes.extend(page.sandbox_environments);
            match page.next_page_token.filter(|token| !token.is_empty()) {
                Some(token) => page_token = Some(token),
                None => return Ok(sandboxes),
            }
        }
        Err(errors.invalid_response(format!(
            "vertex sandbox list returned page tokens for more than {LIST_MAX_PAGES} pages; refusing to paginate further",
        )))
    }

    /// Runs `:execute` on a sandbox with raw chunks. Synchronous — no LRO.
    ///
    /// Every `:execute` call resets the sandbox TTL server-side. The
    /// request is rejected before sending when the decoded chunk payloads
    /// exceed the 100 MB per-request file limit.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is outside the configured scope, the
    /// payload exceeds the request limit, the request fails, or the
    /// response is not an `:execute` response.
    pub async fn execute(&self, name: &str, inputs: Vec<Chunk>) -> Result<Vec<Chunk>> {
        let errors = self.http.errors();
        self.validate_sandbox_name(name)?;
        // base64 decodes to at most 3 bytes per 4 encoded characters.
        let decoded_bytes: usize = inputs.iter().map(|chunk| chunk.data.len() / 4 * 3).sum();
        if decoded_bytes > MAX_REQUEST_FILE_BYTES {
            return Err(errors.invalid_input(format!(
                "vertex sandbox :execute payload of ~{decoded_bytes} bytes exceeds the {MAX_REQUEST_FILE_BYTES}-byte (100 MB) per-request limit; split the input files across executions",
            )));
        }
        let request = self
            .http
            .request(Method::POST, &format!("{name}:execute"))
            .await?
            .json(&ExecuteRequest { inputs });
        let value = self.http.send_value(request).await?;
        let response: ExecuteResponse = serde_json::from_value(value).map_err(|error| {
            errors.invalid_response(format!(
                "failed to parse vertex sandbox :execute response: {}",
                truncate_for_error(&error.to_string()),
            ))
        })?;
        Ok(response.outputs)
    }

    /// Runs source code in a sandbox using the code-execution chunk
    /// conventions and decodes the outputs.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_code::vertex_sandbox::{InputFile, VertexSandboxClient, VertexSandboxConfig};
    ///
    /// # async fn run(client: VertexSandboxClient, sandbox: &str) -> adk_core::Result<()> {
    /// let files = [InputFile::new("data.csv", "text/csv", b"a,b\n1,2\n".to_vec())];
    /// let result = client
    ///     .execute_code(sandbox, "print(open('data.csv').read())", &files)
    ///     .await?;
    /// println!("{}", result.stdout);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when a filename is blank, the files exceed the
    /// 100 MB per-request limit, the execution fails, or the outputs
    /// cannot be decoded.
    pub async fn execute_code(
        &self,
        name: &str,
        code: &str,
        input_files: &[InputFile],
    ) -> Result<SandboxExecutionResult> {
        let errors = self.http.errors();
        let file_bytes: usize = input_files.iter().map(|file| file.data.len()).sum();
        if file_bytes > MAX_REQUEST_FILE_BYTES {
            return Err(errors.invalid_input(format!(
                "vertex sandbox input files total {file_bytes} bytes, exceeding the {MAX_REQUEST_FILE_BYTES}-byte (100 MB) per-request limit; split the files across executions",
            )));
        }
        let mut inputs = Vec::with_capacity(input_files.len() + 1);
        inputs.push(encode_code_chunk(code));
        for file in input_files {
            if file.name.trim().is_empty() {
                return Err(errors.invalid_input(
                    "vertex sandbox input file names must not be blank; set InputFile::name to the filename the code should see",
                ));
            }
            inputs.push(encode_file_chunk(file));
        }
        let outputs = self.execute(name, inputs).await?;
        decode_output_chunks(&outputs)
    }

    /// Deletes a sandbox and waits for the delete operation to complete.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is outside the configured scope, the
    /// request fails, or the operation completes with an error or times out.
    pub async fn delete_sandbox(&self, name: &str) -> Result<()> {
        self.validate_sandbox_name(name)?;
        let request = self.http.request(Method::DELETE, name).await?;
        let operation = self.http.send_value(request).await?;
        self.delete_poller
            .wait_for_operation(
                &self.http,
                operation,
                "sandbox delete",
                false,
                &self.project_id,
                &self.location,
            )
            .await?;
        Ok(())
    }

    /// Resolves a full engine resource name or bare numeric ID against the
    /// configured project and location.
    fn resolve_engine(&self, engine: &str) -> Result<VertexResourceName> {
        let errors = self.http.errors();
        if let Some(name) = VertexResourceName::parse(engine) {
            if name.project_id() != self.project_id || name.location() != self.location {
                return Err(errors.invalid_input(format!(
                    "reasoning engine '{}' does not belong to projects/{}/locations/{}; construct the client with a matching VertexSandboxConfig",
                    truncate_for_error(engine),
                    self.project_id,
                    self.location,
                )));
            }
            return Ok(name);
        }
        if is_canonical_reasoning_engine_id(engine) {
            return Ok(VertexResourceName::new(&self.project_id, &self.location, engine));
        }
        Err(errors.invalid_input(format!(
            "reasoning engine '{}' is neither a full projects/*/locations/*/reasoningEngines/* name nor a canonical numeric engine ID",
            truncate_for_error(engine),
        )))
    }

    /// Rejects sandbox names outside the configured scope or with the wrong
    /// shape, so a compromised or buggy server cannot redirect follow-up
    /// requests elsewhere.
    fn validate_sandbox_name(&self, name: &str) -> Result<()> {
        let errors = self.http.errors();
        if !is_scoped_resource_name(name, &self.project_id, &self.location) {
            return Err(errors.invalid_input(format!(
                "sandbox name '{}' does not belong to projects/{}/locations/{}",
                truncate_for_error(name),
                self.project_id,
                self.location,
            )));
        }
        let segments: Vec<&str> = name.split('/').collect();
        let well_formed = matches!(
            segments.as_slice(),
            ["projects", _, "locations", _, "reasoningEngines", engine, "sandboxEnvironments", sandbox]
                if !engine.is_empty() && !sandbox.is_empty()
        );
        if !well_formed {
            return Err(errors.invalid_input(format!(
                "sandbox name '{}' is not a projects/*/locations/*/reasoningEngines/*/sandboxEnvironments/* resource name",
                truncate_for_error(name),
            )));
        }
        Ok(())
    }

    fn parse_sandbox(&self, value: Value) -> Result<SandboxEnvironment> {
        serde_json::from_value(value).map_err(|error| {
            self.http.errors().invalid_response(format!(
                "failed to parse vertex sandbox resource: {}",
                truncate_for_error(&error.to_string()),
            ))
        })
    }
}
