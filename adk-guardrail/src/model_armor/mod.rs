//! Google Cloud Model Armor guardrails.
//!
//! [Model Armor](https://cloud.google.com/security-command-center/docs/model-armor-overview)
//! screens prompts and responses against a template of filters: prompt injection and jailbreak,
//! Responsible AI categories, Sensitive Data Protection, malicious URIs, and CSAM. A
//! [`ModelArmor`] client is bound to one template and hands out three guardrails:
//!
//! | Guardrail | Screens | Model Armor method |
//! |-----------|---------|--------------------|
//! | [`ModelArmor::prompt_guardrail`] | a user message (input guardrails) | `sanitizeUserPrompt` |
//! | [`ModelArmor::response_guardrail`] | model output (output guardrails) | `sanitizeModelResponse` |
//! | [`ModelArmor::tool_result_guardrail`] | a tool's result (tool guardrails) | `sanitizeUserPrompt` |
//!
//! A tool result is screened as a prompt because it becomes input to the model, which is where
//! prompt-injection detection applies.
//!
//! A match fails the content at the configured [`Severity`] (default [`Severity::High`], and
//! always [`Severity::Critical`] for CSAM), so the guardrail set blocks it and the block names
//! the matching filters. When the only match is Sensitive Data Protection and the template
//! de-identifies, the de-identified text replaces the content instead. When Model Armor cannot
//! screen the content — the call fails, or the service reports a partial or failed invocation
//! without a match — [`FailMode::Closed`] (the default) blocks it and [`FailMode::Open`] lets it
//! through as a [`Severity::Low`] failure, which the agent records as a guardrail warning.
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_guardrail::model_armor::{FailMode, ModelArmor};
//! use adk_guardrail::{GuardrailSet, ToolGuardrailSet};
//!
//! # fn main() -> adk_core::Result<()> {
//! let armor =
//!     ModelArmor::builder("projects/my-project/locations/us-central1/templates/agent-screen")
//!         .fail_mode(FailMode::Closed)
//!         .build()?;
//!
//! let input_guardrails = GuardrailSet::new().with(armor.prompt_guardrail());
//! let output_guardrails = GuardrailSet::new().with(armor.response_guardrail());
//! let tool_guardrails = ToolGuardrailSet::new().with(armor.tool_result_guardrail());
//! # let _ = (input_guardrails, output_guardrails, tool_guardrails);
//! # Ok(())
//! # }
//! ```

mod types;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Content, ErrorComponent, Part};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient};
use async_trait::async_trait;
use serde_json::{Value, json};

pub use google_cloud_auth::credentials::Credentials;
pub use types::{
    DataItem, DetectionConfidenceLevel, FilterExecutionState, FilterMatchState, FilterResult,
    FilterVerdict, InvocationResult, MatchedFilter, MessageItem, RaiFilterResult,
    SanitizationMetadata, SanitizationResult, SdpDeidentifyResult, SdpFilterResult, SdpFinding,
    SdpInspectResult,
};

use crate::{Guardrail, GuardrailResult, Severity, ToolGuardrail, ToolResultGuardrailResult};

/// The default name of a Model Armor guardrail, as blocks and warnings report it.
pub const MODEL_ARMOR_GUARDRAIL: &str = "model_armor";

const API_VERSION: &str = "v1";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

const ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "guardrail.model_armor.invalid_input",
    unauthorized: "guardrail.model_armor.unauthorized",
    forbidden: "guardrail.model_armor.forbidden",
    not_found: "guardrail.model_armor.not_found",
    rate_limited: "guardrail.model_armor.rate_limited",
    timeout: "guardrail.model_armor.timeout",
    unavailable: "guardrail.model_armor.unavailable",
    credentials_unavailable: "guardrail.model_armor.credentials_unavailable",
    invalid_response: "guardrail.model_armor.invalid_response",
    invalid_request: "guardrail.model_armor.invalid_request",
    upstream_error: "guardrail.model_armor.upstream_error",
    operation_failed: "guardrail.model_armor.operation_failed",
};

/// What a Model Armor guardrail does with content it could not screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FailMode {
    /// Blocks the content, at the guardrail's severity. A tool result is withheld.
    #[default]
    Closed,
    /// Lets the content through as a [`Severity::Low`] failure, which an agent records as a
    /// guardrail warning. A tool result is delivered and the failure is logged.
    Open,
}

/// A Model Armor template's resource name,
/// `projects/{project}/locations/{location}/templates/{template}`.
///
/// # Example
///
/// ```rust
/// use adk_guardrail::model_armor::TemplateName;
///
/// let name: TemplateName = "projects/p/locations/europe-west4/templates/t".parse().unwrap();
/// assert_eq!(name.location(), "europe-west4");
/// assert!("templates/t".parse::<TemplateName>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateName {
    name: String,
    location: String,
}

impl TemplateName {
    /// The full resource name.
    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// The template's region, which selects the regional endpoint.
    pub fn location(&self) -> &str {
        &self.location
    }

    /// The template's regional endpoint, `https://modelarmor.{location}.rep.googleapis.com`.
    pub fn regional_endpoint(&self) -> String {
        format!("https://modelarmor.{}.rep.googleapis.com", self.location)
    }
}

impl std::str::FromStr for TemplateName {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let segments: Vec<&str> = name.split('/').collect();
        let valid_segment = |segment: &str| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        match segments.as_slice() {
            ["projects", project, "locations", location, "templates", template]
                if valid_segment(project)
                    && valid_segment(template)
                    && !location.is_empty()
                    && location
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') =>
            {
                Ok(Self { name: name.to_string(), location: (*location).to_string() })
            }
            _ => Err(format!(
                "'{name}' is not a Model Armor template name; use the full resource name \
                 projects/{{project}}/locations/{{location}}/templates/{{template}}"
            )),
        }
    }
}

/// Builder for [`ModelArmor`].
#[derive(Debug)]
pub struct ModelArmorBuilder {
    template: String,
    endpoint: Option<String>,
    credentials: Option<Credentials>,
    fail_mode: FailMode,
    severity: Severity,
    name: String,
    request_timeout: Duration,
}

impl ModelArmorBuilder {
    /// Sends requests to `endpoint` instead of the template's regional endpoint, such as a
    /// private endpoint or a local test server. It must be an HTTPS origin, or HTTP to a
    /// loopback host.
    #[must_use]
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Uses `credentials` instead of Application Default Credentials.
    #[must_use]
    pub fn credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Sets what the guardrails do with content Model Armor could not screen. Defaults to
    /// [`FailMode::Closed`].
    #[must_use]
    pub fn fail_mode(mut self, fail_mode: FailMode) -> Self {
        self.fail_mode = fail_mode;
        self
    }

    /// Sets the severity of a match. Defaults to [`Severity::High`]; a CSAM match is always
    /// [`Severity::Critical`].
    #[must_use]
    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// Sets the guardrail name blocks and warnings report. Defaults to
    /// [`MODEL_ARMOR_GUARDRAIL`].
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sets the per-request timeout. Defaults to 15 seconds.
    #[must_use]
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Validates the template name and endpoint, resolves credentials, and builds the client.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidInput` error with code `guardrail.model_armor.invalid_input` when the
    /// template is not a full resource name or the endpoint is not a secure origin, and an
    /// `Unauthorized` error when Application Default Credentials cannot be built.
    pub fn build(self) -> adk_core::Result<ModelArmor> {
        let errors = GcpErrorContext::new(ErrorComponent::Guardrail, ERROR_CODES, "model armor")
            .with_provider("model_armor");
        let template: TemplateName =
            self.template.parse().map_err(|message: String| errors.invalid_input(message))?;
        let endpoint = self.endpoint.unwrap_or_else(|| template.regional_endpoint());
        let mut http = GcpHttpClient::builder(errors, endpoint)
            .api_version(API_VERSION)
            .request_timeout(self.request_timeout)
            .max_response_bytes(MAX_RESPONSE_BYTES);
        if let Some(credentials) = self.credentials {
            http = http.credentials(credentials);
        }
        Ok(ModelArmor {
            inner: Arc::new(Inner {
                http: http.build()?,
                template,
                fail_mode: self.fail_mode,
                severity: self.severity,
                name: self.name,
            }),
        })
    }
}

/// A Model Armor client bound to one template.
///
/// Cloning is cheap; every guardrail it hands out shares its HTTP client and credentials.
#[derive(Debug, Clone)]
pub struct ModelArmor {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: GcpHttpClient,
    template: TemplateName,
    fail_mode: FailMode,
    severity: Severity,
    name: String,
}

/// Which Model Armor method screens the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    UserPrompt,
    ModelResponse,
}

impl Method {
    fn path_suffix(self) -> &'static str {
        match self {
            Self::UserPrompt => "sanitizeUserPrompt",
            Self::ModelResponse => "sanitizeModelResponse",
        }
    }
}

impl ModelArmor {
    /// Starts building a client for `template`, the full resource name
    /// `projects/{project}/locations/{location}/templates/{template}`.
    pub fn builder(template: impl Into<String>) -> ModelArmorBuilder {
        ModelArmorBuilder {
            template: template.into(),
            endpoint: None,
            credentials: None,
            fail_mode: FailMode::default(),
            severity: Severity::High,
            name: MODEL_ARMOR_GUARDRAIL.to_string(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// The template this client screens against.
    pub fn template(&self) -> &TemplateName {
        &self.inner.template
    }

    /// Screens `text` as a user prompt with `sanitizeUserPrompt`.
    ///
    /// # Errors
    ///
    /// Returns the transport, status, or parse error, with component `Guardrail` and a
    /// `guardrail.model_armor.*` code.
    pub async fn sanitize_user_prompt(&self, text: &str) -> adk_core::Result<SanitizationResult> {
        self.sanitize(Method::UserPrompt, text).await
    }

    /// Screens `text` as a model response with `sanitizeModelResponse`.
    ///
    /// # Errors
    ///
    /// Returns the transport, status, or parse error, with component `Guardrail` and a
    /// `guardrail.model_armor.*` code.
    pub async fn sanitize_model_response(
        &self,
        text: &str,
    ) -> adk_core::Result<SanitizationResult> {
        self.sanitize(Method::ModelResponse, text).await
    }

    /// A content guardrail that screens user messages with `sanitizeUserPrompt`, for an
    /// agent's input guardrails.
    pub fn prompt_guardrail(&self) -> ModelArmorGuardrail {
        ModelArmorGuardrail { armor: self.clone(), method: Method::UserPrompt }
    }

    /// A content guardrail that screens model output with `sanitizeModelResponse`, for an
    /// agent's output guardrails.
    pub fn response_guardrail(&self) -> ModelArmorGuardrail {
        ModelArmorGuardrail { armor: self.clone(), method: Method::ModelResponse }
    }

    /// A tool guardrail that screens every tool result with `sanitizeUserPrompt` before the
    /// model sees it.
    pub fn tool_result_guardrail(&self) -> ModelArmorToolGuardrail {
        ModelArmorToolGuardrail { armor: self.clone(), tools: None }
    }

    async fn sanitize(&self, method: Method, text: &str) -> adk_core::Result<SanitizationResult> {
        let inner = &self.inner;
        let body = match method {
            Method::UserPrompt => json!({ "userPromptData": { "text": text } }),
            Method::ModelResponse => json!({ "modelResponseData": { "text": text } }),
        };
        let path = format!("{}:{}", inner.template.as_str(), method.path_suffix());
        let request = inner.http.request(reqwest::Method::POST, &path).await?.json(&body);
        let mut response = inner.http.send_value(request).await?;
        let result = response.get_mut("sanitizationResult").map(Value::take).ok_or_else(|| {
            inner.http.errors().invalid_response(
                "model armor response carries no sanitizationResult; check the template name",
            )
        })?;
        serde_json::from_value(result).map_err(|error| {
            inner
                .http
                .errors()
                .invalid_response(format!("model armor sanitizationResult does not parse: {error}"))
        })
    }

    /// Screens `text` and turns the result into what a guardrail decides.
    async fn screen(&self, method: Method, text: &str) -> Screening {
        let inner = &self.inner;
        let result = match self.sanitize(method, text).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(
                    guardrail.name = %inner.name,
                    model_armor.template = %inner.template.as_str(),
                    error.code = %error.code,
                    "model armor could not screen content"
                );
                return Screening::Unscreened(format!(
                    "Model Armor could not screen the content ({})",
                    error.code
                ));
            }
        };
        let matched = result.matched_filters();
        tracing::info!(
            guardrail.name = %inner.name,
            model_armor.template = %inner.template.as_str(),
            model_armor.method = method.path_suffix(),
            model_armor.invocation = ?result.invocation_result,
            model_armor.matched = %describe(&matched),
            "model armor screened content"
        );
        if !matched.is_empty() || result.filter_match_state == FilterMatchState::MatchFound {
            if let Some(text) = result.deidentified_text() {
                return Screening::Deidentified(text.to_string());
            }
            let severity = if matched.iter().any(|m| m.filter == "csam") {
                Severity::Critical
            } else {
                inner.severity
            };
            return Screening::Matched {
                reason: format!(
                    "Model Armor template {} matched: {}",
                    inner.template.as_str(),
                    describe(&matched)
                ),
                severity,
            };
        }
        if result.invocation_result != InvocationResult::Success {
            let detail = result
                .sanitization_metadata
                .as_ref()
                .and_then(|metadata| metadata.error_code.as_deref())
                .map(|code| format!(", error code {code}"))
                .unwrap_or_default();
            return Screening::Unscreened(format!(
                "Model Armor screening was incomplete (invocation {:?}{detail})",
                result.invocation_result
            ));
        }
        Screening::Clean
    }
}

/// What one screening decided.
enum Screening {
    Clean,
    Matched { reason: String, severity: Severity },
    Deidentified(String),
    Unscreened(String),
}

fn describe(matched: &[MatchedFilter]) -> String {
    if matched.is_empty() {
        return "none".to_string();
    }
    matched.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

/// A content [`Guardrail`] backed by Model Armor, from [`ModelArmor::prompt_guardrail`] or
/// [`ModelArmor::response_guardrail`].
///
/// It screens the text parts of the content, joined by newlines; content with no text passes
/// without a request.
#[derive(Debug, Clone)]
pub struct ModelArmorGuardrail {
    armor: ModelArmor,
    method: Method,
}

#[async_trait]
impl Guardrail for ModelArmorGuardrail {
    fn name(&self) -> &str {
        &self.armor.inner.name
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text: Vec<&str> = content.parts.iter().filter_map(Part::text).collect();
        let text = text.join("\n");
        if text.trim().is_empty() {
            return GuardrailResult::Pass;
        }
        match self.armor.screen(self.method, &text).await {
            Screening::Clean => GuardrailResult::Pass,
            Screening::Matched { reason, severity } => GuardrailResult::fail(reason, severity),
            Screening::Deidentified(text) => {
                let mut parts = Vec::with_capacity(content.parts.len());
                let mut replacement = Some(text);
                for part in &content.parts {
                    match part {
                        Part::Text { .. } => {
                            if let Some(text) = replacement.take() {
                                parts.push(Part::Text { text });
                            }
                        }
                        other => parts.push(other.clone()),
                    }
                }
                GuardrailResult::transform(
                    Content { role: content.role.clone(), parts },
                    "Model Armor de-identified sensitive data",
                )
            }
            Screening::Unscreened(reason) => match self.armor.inner.fail_mode {
                FailMode::Closed => GuardrailResult::fail(reason, self.armor.inner.severity),
                FailMode::Open => GuardrailResult::fail(reason, Severity::Low),
            },
        }
    }
}

/// A [`ToolGuardrail`] that screens tool results with Model Armor, from
/// [`ModelArmor::tool_result_guardrail`].
///
/// A string result is screened as is; any other result as its JSON encoding. A match withholds
/// the result, and a Sensitive Data Protection de-identification replaces it (parsed back to
/// JSON when the de-identified text still parses).
#[derive(Debug, Clone)]
pub struct ModelArmorToolGuardrail {
    armor: ModelArmor,
    tools: Option<Vec<String>>,
}

impl ModelArmorToolGuardrail {
    /// Restricts screening to the named tools. Without this every tool's result is screened.
    #[must_use]
    pub fn on_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }
}

#[async_trait]
impl ToolGuardrail for ModelArmorToolGuardrail {
    fn name(&self) -> &str {
        &self.armor.inner.name
    }

    fn applies_to(&self, tool_name: &str) -> bool {
        self.tools.as_ref().is_none_or(|tools| tools.iter().any(|tool| tool == tool_name))
    }

    async fn validate_result(
        &self,
        tool_name: &str,
        _args: &Value,
        result: &Value,
    ) -> ToolResultGuardrailResult {
        let text = match result {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        if text.trim().is_empty() {
            return ToolResultGuardrailResult::Allow;
        }
        match self.armor.screen(Method::UserPrompt, &text).await {
            Screening::Clean => ToolResultGuardrailResult::Allow,
            Screening::Matched { reason, severity } => {
                ToolResultGuardrailResult::withhold(reason, severity)
            }
            Screening::Deidentified(text) => {
                let revised = match result {
                    Value::String(_) => Value::String(text),
                    _ => serde_json::from_str(&text).unwrap_or(Value::String(text)),
                };
                ToolResultGuardrailResult::revise(
                    revised,
                    "Model Armor de-identified sensitive data",
                )
            }
            Screening::Unscreened(reason) => match self.armor.inner.fail_mode {
                FailMode::Closed => {
                    ToolResultGuardrailResult::withhold(reason, self.armor.inner.severity)
                }
                FailMode::Open => {
                    tracing::warn!(
                        guardrail.name = %self.armor.inner.name,
                        tool.name = tool_name,
                        "model armor could not screen a tool result; delivering it unscreened (fail open)"
                    );
                    ToolResultGuardrailResult::Allow
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_names_must_be_full_resource_names() {
        let name: TemplateName =
            "projects/my-proj/locations/us-central1/templates/screen_1".parse().unwrap();
        assert_eq!(name.location(), "us-central1");
        assert_eq!(name.regional_endpoint(), "https://modelarmor.us-central1.rep.googleapis.com");

        for invalid in [
            "screen_1",
            "templates/screen_1",
            "projects/p/locations/us-central1/templates/",
            "projects/p/locations/US/templates/t",
            "projects/p/locations/us-central1/templates/t/extra",
            "projects/p/locations/us-central1.evil.com/templates/t",
            "projects/p q/locations/us-central1/templates/t",
        ] {
            assert!(invalid.parse::<TemplateName>().is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn an_invalid_template_fails_the_build_with_a_guardrail_error() {
        let error = ModelArmor::builder("my-template").build().unwrap_err();
        assert_eq!(error.component, ErrorComponent::Guardrail);
        assert_eq!(error.category, adk_core::ErrorCategory::InvalidInput);
        assert_eq!(error.code, "guardrail.model_armor.invalid_input");
    }

    #[test]
    fn fail_closed_is_the_default() {
        assert_eq!(FailMode::default(), FailMode::Closed);
        assert_eq!(ModelArmor::builder("x").fail_mode, FailMode::Closed);
    }
}
