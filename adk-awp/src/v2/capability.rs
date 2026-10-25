//! Capabilities as `adk_core::Tool`s: one declaration, three bindings, one governed path
//! (spec 3.5 §7.4, D-4).
//!
//! A [`CapabilityTool`] wraps an owner's [`CapabilityHandler`] as an `adk_core::Tool` whose
//! `effect()` is the declared [`CapabilityEffect`] and whose `origin()` is `awp:{domain}`. It
//! validates its input against the declared schema before the handler runs. A
//! [`CapabilityRegistry`] holds a site's capabilities and builds the manifest for a tier.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use awp_types::{
    ApprovalHint, CapabilityEffect, CapabilityManifestV2, CapabilityV2, ErrorBody, ErrorCode,
    RequesterId, TrustTier,
};
use serde_json::{Value, json};
use uuid::Uuid;

/// Request-metadata key holding the caller's verified requester id.
pub const META_REQUESTER: &str = "awp.requester";
/// Request-metadata key holding the caller's tier.
pub const META_TIER: &str = "awp.tier";
/// Request-metadata key holding the consent receipt ids the call references.
pub const META_CONSENT: &str = "awp.consent";
/// `adk_core::AdkError` metadata key carrying the AWP 2.0 error code of a refused call.
pub const META_ERROR_CODE: &str = "awp_error_code";
/// `$id` of the held-call schema, for the output schema of capabilities that may hold.
pub const HELD_CALL_SCHEMA_ID: &str =
    "https://agenticwebprotocol.com/schemas/2.0/held-call.schema.json";

/// One call of a capability, as its handler sees it (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityCall {
    /// The capability's name.
    pub capability: String,
    /// The input object, already validated against the input schema.
    pub input: Value,
    /// The caller's tier.
    pub tier: TrustTier,
    /// The caller's verified requester id, from `known` up.
    pub requester: Option<RequesterId>,
    /// The call's idempotency key; a `NonIdempotent` handler passes it downstream.
    pub idempotency_key: String,
    /// Consent receipt ids the call references.
    pub consent: Vec<Uuid>,
}

/// Why a capability handler did not produce an output (spec 3.5 §7.4, §7.5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityError {
    /// The call is refused with an AWP error code the requester sees, such as
    /// `inventory_unavailable` or `quote_expired`.
    #[error("{message}")]
    Refused {
        /// The AWP 2.0 error code.
        code: ErrorCode,
        /// A message with no internal detail.
        message: String,
    },
    /// The handler failed; the requester sees `internal_error` and the detail is logged.
    #[error("capability handler failed: {0}")]
    Failed(String),
}

impl CapabilityError {
    /// A refusal with `code` and `message`.
    pub fn refused(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Refused { code, message: message.into() }
    }
}

/// The owner's code behind a capability (spec 3.5 §7.4).
///
/// # Example
///
/// ```
/// use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler};
/// use serde_json::{Value, json};
///
/// struct Quote;
///
/// #[async_trait::async_trait]
/// impl CapabilityHandler for Quote {
///     async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
///         let panels = call.input["panels"].as_u64().unwrap_or(1);
///         Ok(json!({ "total": panels * 25_000, "currency": "KES" }))
///     }
/// }
/// ```
#[async_trait]
pub trait CapabilityHandler: Send + Sync {
    /// Executes the call.
    ///
    /// # Errors
    ///
    /// [`CapabilityError::Refused`] for an outcome the requester should see as an AWP error;
    /// [`CapabilityError::Failed`] for anything else.
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError>;

    /// The amount of a call in minor units, compared with an `above_amount` approval hint.
    ///
    /// The default reads a non-negative integer `amount` member of the input.
    fn call_amount(&self, input: &Value) -> Option<u64> {
        input.get("amount").and_then(Value::as_u64)
    }
}

/// Why a capability cannot be declared (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityDefinitionError {
    /// The name is not `^[a-z][a-z0-9_]{0,63}$`.
    #[error(
        "capability name `{0}` must match ^[a-z][a-z0-9_]{{0,63}}$: lowercase letters, digits and underscores"
    )]
    InvalidName(String),
    /// The input schema does not compile.
    #[error("capability `{name}` has an invalid input schema: {reason}")]
    InvalidSchema {
        /// The capability's name.
        name: String,
        /// Why the schema is invalid.
        reason: String,
    },
    /// Two capabilities share a name.
    #[error("capability `{0}` is declared twice")]
    Duplicate(String),
}

/// Returns `true` when `name` is a valid capability name: `^[a-z][a-z0-9_]{0,63}$`.
pub fn is_valid_capability_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && name.len() <= 64
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A capability exposed as an `adk_core::Tool` (spec 3.5 §7.4, D-4).
///
/// # Example
///
/// ```
/// use std::sync::Arc;
///
/// use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityTool};
/// use adk_core::{Tool, ToolEffect};
/// use awp_types::{CapabilityEffect, CapabilityV2};
/// use serde_json::{Value, json};
///
/// struct Echo;
///
/// #[async_trait::async_trait]
/// impl CapabilityHandler for Echo {
///     async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
///         Ok(call.input)
///     }
/// }
///
/// let mut declaration = CapabilityV2::new("place_order", "Place an order", CapabilityEffect::NonIdempotent);
/// declaration.input_schema = Some(json!({ "type": "object", "required": ["sku"] }));
/// let tool = CapabilityTool::new("example.com", declaration, Arc::new(Echo)).unwrap();
/// assert_eq!(tool.origin(), "awp:example.com");
/// assert_eq!(tool.effect(), ToolEffect::NonIdempotent);
/// assert!(tool.validate_input(&json!({ "sku": "PANEL-400" })).is_ok());
/// assert!(tool.validate_input(&json!({})).is_err());
/// ```
pub struct CapabilityTool {
    declaration: CapabilityV2,
    origin: String,
    handler: Arc<dyn CapabilityHandler>,
    validator: Option<jsonschema::Validator>,
    annotations: Value,
    response_schema: Option<Value>,
}

impl std::fmt::Debug for CapabilityTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityTool")
            .field("name", &self.declaration.name)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl CapabilityTool {
    /// Declares `declaration` for the site at `domain`, executed by `handler`.
    ///
    /// # Errors
    ///
    /// [`CapabilityDefinitionError`] when the name is invalid or the input schema does not
    /// compile.
    pub fn new(
        domain: &str,
        declaration: CapabilityV2,
        handler: Arc<dyn CapabilityHandler>,
    ) -> Result<Self, CapabilityDefinitionError> {
        if !is_valid_capability_name(&declaration.name) {
            return Err(CapabilityDefinitionError::InvalidName(declaration.name));
        }
        let validator = declaration
            .input_schema
            .as_ref()
            .map(|schema| {
                jsonschema::validator_for(schema).map_err(|error| {
                    CapabilityDefinitionError::InvalidSchema {
                        name: declaration.name.clone(),
                        reason: error.to_string(),
                    }
                })
            })
            .transpose()?;
        let annotations = json!({
            "readOnlyHint": declaration.effect == CapabilityEffect::ReadOnly,
            "idempotentHint": declaration.effect != CapabilityEffect::NonIdempotent,
        });
        // A call that may be held answers with the held-call document instead of the output.
        let response_schema = match (&declaration.approval, &declaration.output_schema) {
            (ApprovalHint::Never, output) => output.clone(),
            (_, output) => Some(json!({
                "oneOf": [
                    output.clone().unwrap_or_else(|| json!({ "type": "object" })),
                    { "$ref": HELD_CALL_SCHEMA_ID },
                ]
            })),
        };
        Ok(Self {
            origin: Self::origin_for(domain),
            declaration,
            handler,
            validator,
            annotations,
            response_schema,
        })
    }

    /// The tool origin of a capability of the site at `domain`: `awp:{domain}`.
    pub fn origin_for(domain: &str) -> String {
        format!("awp:{domain}")
    }

    /// Returns the declaration.
    pub fn declaration(&self) -> &CapabilityV2 {
        &self.declaration
    }

    /// Validates `input` against the declared input schema; the input must be an object.
    ///
    /// # Errors
    ///
    /// The validation messages, at most five.
    pub fn validate_input(&self, input: &Value) -> Result<(), Vec<String>> {
        if !input.is_object() {
            return Err(vec!["the input must be a JSON object".to_owned()]);
        }
        match &self.validator {
            None => Ok(()),
            Some(validator) => {
                let errors: Vec<String> =
                    validator.iter_errors(input).take(5).map(|error| error.to_string()).collect();
                if errors.is_empty() { Ok(()) } else { Err(errors) }
            }
        }
    }

    /// The amount of a call in minor units, for an `above_amount` approval hint: the
    /// handler's reading of the input, else the declared price.
    pub fn call_amount(&self, input: &Value) -> Option<u64> {
        self.handler
            .call_amount(input)
            .or_else(|| self.declaration.price.as_ref().map(|price| price.amount))
    }

    /// Runs the handler on validated input.
    ///
    /// # Errors
    ///
    /// [`CapabilityError`] from the handler, or `schema_validation_failed` for invalid input.
    pub async fn invoke(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        if let Err(errors) = self.validate_input(&call.input) {
            return Err(CapabilityError::refused(
                ErrorCode::SchemaValidationFailed,
                format!("the input does not match the input schema: {}", errors.join("; ")),
            ));
        }
        self.handler.handle(call).await
    }
}

/// Converts a capability error into the `AdkError` a tool returns, carrying its AWP code.
pub fn capability_error_to_adk(error: &CapabilityError) -> AdkError {
    let (code, message) = match error {
        CapabilityError::Refused { code, message } => (code.clone(), message.clone()),
        CapabilityError::Failed(detail) => {
            tracing::warn!(error = %detail, "awp capability handler failed");
            (ErrorCode::InternalError, "the capability failed".to_owned())
        }
    };
    let category = match code.http_status() {
        400 | 409 | 410 | 451 => ErrorCategory::InvalidInput,
        401 => ErrorCategory::Unauthorized,
        402 | 403 => ErrorCategory::Forbidden,
        404 => ErrorCategory::NotFound,
        429 => ErrorCategory::RateLimited,
        502 | 503 => ErrorCategory::Unavailable,
        _ => ErrorCategory::Internal,
    };
    let mut adk = AdkError::new(ErrorComponent::Tool, category, "awp.capability.refused", message);
    adk.details.metadata.insert(META_ERROR_CODE.to_owned(), Value::String(code.to_string()));
    adk
}

/// Converts a tool error into the AWP error body a requester sees: the AWP code a
/// capability set, else one inferred from the category, with no internal detail.
pub fn adk_error_to_body(error: &AdkError) -> ErrorBody {
    if let Some(Value::String(code)) = error.details.metadata.get(META_ERROR_CODE) {
        return ErrorBody::new(ErrorCode::from(code.as_str()), error.message.clone());
    }
    let code = match error.category {
        ErrorCategory::InvalidInput => ErrorCode::InvalidRequest,
        ErrorCategory::Unauthorized => ErrorCode::Unauthorized,
        ErrorCategory::Forbidden => ErrorCode::Forbidden,
        ErrorCategory::NotFound => ErrorCode::ResourceNotFound,
        ErrorCategory::RateLimited => ErrorCode::RateLimitExceeded,
        ErrorCategory::Timeout | ErrorCategory::Unavailable => ErrorCode::ServiceDegraded,
        _ => ErrorCode::InternalError,
    };
    tracing::warn!(error.code = error.code, error = %error.message, "awp capability tool failed");
    ErrorBody::new(code.clone(), format!("the capability failed ({code})"))
}

#[async_trait]
impl Tool for CapabilityTool {
    fn name(&self) -> &str {
        &self.declaration.name
    }

    fn description(&self) -> &str {
        &self.declaration.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.declaration.input_schema.clone().unwrap_or_else(|| json!({ "type": "object" })))
    }

    fn response_schema(&self) -> Option<Value> {
        self.response_schema.clone()
    }

    fn annotations(&self) -> Option<&Value> {
        Some(&self.annotations)
    }

    fn origin(&self) -> &str {
        &self.origin
    }

    fn effect(&self) -> ToolEffect {
        match self.declaration.effect {
            CapabilityEffect::ReadOnly => ToolEffect::ReadOnly,
            CapabilityEffect::Idempotent => ToolEffect::Idempotent,
            _ => ToolEffect::NonIdempotent,
        }
    }

    fn is_read_only(&self) -> bool {
        self.declaration.effect == CapabilityEffect::ReadOnly
    }

    fn is_concurrency_safe(&self) -> bool {
        self.declaration.effect == CapabilityEffect::ReadOnly
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let metadata = ctx.request_metadata();
        let requester = metadata.get(META_REQUESTER).and_then(Value::as_str).map(RequesterId::new);
        let tier = metadata
            .get(META_TIER)
            .and_then(|tier| serde_json::from_value(tier.clone()).ok())
            .unwrap_or(TrustTier::Public);
        let consent = metadata
            .get(META_CONSENT)
            .and_then(|ids| serde_json::from_value(ids.clone()).ok())
            .unwrap_or_default();
        let call = CapabilityCall {
            capability: self.declaration.name.clone(),
            input: args,
            tier,
            requester,
            idempotency_key: ctx.idempotency_key(),
            consent,
        };
        self.invoke(call).await.map_err(|error| capability_error_to_adk(&error))
    }
}

/// A site's capabilities, by name (spec 3.5 §7.4).
///
/// # Example
///
/// ```
/// use std::sync::Arc;
///
/// use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry, CapabilityTool};
/// use awp_types::{CapabilityEffect, CapabilityV2, TrustTier};
/// use serde_json::Value;
///
/// struct Noop;
/// #[async_trait::async_trait]
/// impl CapabilityHandler for Noop {
///     async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> { Ok(call.input) }
/// }
///
/// let mut registry = CapabilityRegistry::new("example.com");
/// registry.declare(CapabilityV2::new("search_site", "Search", CapabilityEffect::ReadOnly), Arc::new(Noop)).unwrap();
/// registry
///     .declare(
///         CapabilityV2::new("book", "Book a visit", CapabilityEffect::NonIdempotent).with_min_trust(TrustTier::Known),
///         Arc::new(Noop),
///     )
///     .unwrap();
/// assert_eq!(registry.manifest_for(TrustTier::Public).capabilities.len(), 1);
/// assert_eq!(registry.manifest_for(TrustTier::Known).capabilities.len(), 2);
/// ```
#[derive(Debug, Clone)]
pub struct CapabilityRegistry {
    domain: String,
    tools: BTreeMap<String, Arc<CapabilityTool>>,
}

impl CapabilityRegistry {
    /// Creates an empty registry for the site at `domain`.
    pub fn new(domain: impl Into<String>) -> Self {
        Self { domain: domain.into(), tools: BTreeMap::new() }
    }

    /// The site's domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Declares a capability executed by `handler`.
    ///
    /// # Errors
    ///
    /// As [`CapabilityTool::new`], and [`CapabilityDefinitionError::Duplicate`] for a name
    /// already declared.
    pub fn declare(
        &mut self,
        declaration: CapabilityV2,
        handler: Arc<dyn CapabilityHandler>,
    ) -> Result<Arc<CapabilityTool>, CapabilityDefinitionError> {
        if self.tools.contains_key(&declaration.name) {
            return Err(CapabilityDefinitionError::Duplicate(declaration.name));
        }
        let tool = Arc::new(CapabilityTool::new(&self.domain, declaration, handler)?);
        self.tools.insert(tool.declaration().name.clone(), Arc::clone(&tool));
        Ok(tool)
    }

    /// Returns capability `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<CapabilityTool>> {
        self.tools.get(name)
    }

    /// Returns capability `name` when a caller of `tier` may see it; a capability above the
    /// tier is reported exactly as one that does not exist (AWP 2.0 §7.3).
    pub fn visible(&self, name: &str, tier: TrustTier) -> Option<&Arc<CapabilityTool>> {
        self.get(name).filter(|tool| tool.declaration().visible_to(tier))
    }

    /// Returns the capabilities as `adk_core::Tool`s, for the A2A and MCP bindings.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.values().map(|tool| Arc::clone(tool) as Arc<dyn Tool>).collect()
    }

    /// Returns the tools a caller of `tier` may see.
    pub fn tools_for(&self, tier: TrustTier) -> Vec<Arc<dyn Tool>> {
        self.tools
            .values()
            .filter(|tool| tool.declaration().visible_to(tier))
            .map(|tool| Arc::clone(tool) as Arc<dyn Tool>)
            .collect()
    }

    /// The unfiltered manifest: every declaration.
    pub fn manifest(&self) -> CapabilityManifestV2 {
        CapabilityManifestV2::new(
            self.tools.values().map(|tool| tool.declaration().clone()).collect(),
        )
    }

    /// The manifest a caller of `tier` sees (spec 3.5 §7.4).
    pub fn manifest_for(&self, tier: TrustTier) -> CapabilityManifestV2 {
        self.manifest().visible_to(tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::executor::{Binding, EdgeToolContext, Invocation};

    struct Echo;

    #[async_trait]
    impl CapabilityHandler for Echo {
        async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
            Ok(json!({
                "input": call.input,
                "tier": call.tier,
                "requester": call.requester.map(|r| r.to_string()),
                "consent": call.consent,
                "key": call.idempotency_key,
            }))
        }
    }

    #[test]
    fn names_follow_the_protocol_pattern() {
        for good in ["a", "search_site", "book2", &"x".repeat(64)] {
            assert!(is_valid_capability_name(good), "{good}");
        }
        for bad in ["", "Search", "2book", "place-order", "with space", &"x".repeat(65)] {
            assert!(!is_valid_capability_name(bad), "{bad}");
        }
        let error = CapabilityTool::new(
            "example.com",
            CapabilityV2::new("Bad-Name", "x", CapabilityEffect::ReadOnly),
            Arc::new(Echo),
        )
        .unwrap_err();
        assert_eq!(error, CapabilityDefinitionError::InvalidName("Bad-Name".to_owned()));
    }

    /// The MCP binding's hints and output schema follow the declaration (AWP 2.0 §7.4.3, C10).
    #[test]
    fn tool_metadata_mirrors_the_declaration() {
        let read = CapabilityTool::new(
            "example.com",
            CapabilityV2::new("search_site", "Search", CapabilityEffect::ReadOnly),
            Arc::new(Echo),
        )
        .unwrap();
        assert_eq!(
            read.annotations(),
            Some(&json!({ "readOnlyHint": true, "idempotentHint": true }))
        );
        assert_eq!(read.effect(), ToolEffect::ReadOnly);
        assert!(read.is_read_only() && read.is_concurrency_safe());
        assert_eq!(read.parameters_schema(), Some(json!({ "type": "object" })));
        assert_eq!(read.response_schema(), None);

        let mut order = CapabilityV2::new("place_order", "Order", CapabilityEffect::NonIdempotent)
            .with_approval(ApprovalHint::Always);
        order.output_schema = Some(json!({ "type": "object", "required": ["orderId"] }));
        let order = CapabilityTool::new("example.com", order, Arc::new(Echo)).unwrap();
        assert_eq!(
            order.annotations(),
            Some(&json!({ "readOnlyHint": false, "idempotentHint": false }))
        );
        assert_eq!(
            order.response_schema(),
            Some(json!({ "oneOf": [
                { "type": "object", "required": ["orderId"] },
                { "$ref": HELD_CALL_SCHEMA_ID }
            ] }))
        );
        assert_eq!(order.origin(), "awp:example.com");
        let bad_schema = CapabilityV2 {
            input_schema: Some(json!({ "type": 7 })),
            ..CapabilityV2::new("x", "x", CapabilityEffect::ReadOnly)
        };
        assert!(matches!(
            CapabilityTool::new("example.com", bad_schema, Arc::new(Echo)),
            Err(CapabilityDefinitionError::InvalidSchema { .. })
        ));
    }

    /// As an `adk_core::Tool`, the capability reads the caller from the edge's request
    /// metadata and refuses invalid input with its AWP code.
    #[tokio::test]
    async fn the_tool_path_carries_the_caller_and_the_awp_code() {
        let mut declaration = CapabilityV2::new("book", "Book", CapabilityEffect::Idempotent);
        declaration.input_schema = Some(json!({ "type": "object", "required": ["date"] }));
        let tool =
            Arc::new(CapabilityTool::new("example.com", declaration, Arc::new(Echo)).unwrap());
        let consent = Uuid::now_v7();
        let invocation = Invocation {
            tier: TrustTier::Known,
            requester: Some(RequesterId::web_bot_auth("https://buyer.example", "k")),
            idempotency_key: Some("key-1".to_owned()),
            consent: vec![consent],
            ..Invocation::new("book", json!({ "date": "2026-11-02" }), Binding::Mcp)
        };
        let context = Arc::new(EdgeToolContext::new(&tool, &invocation, "call-1"));
        let output =
            Arc::clone(&tool).execute(context.clone(), invocation.input.clone()).await.unwrap();
        assert_eq!(
            output,
            json!({
                "input": { "date": "2026-11-02" },
                "tier": "known",
                "requester": "wba:https://buyer.example#k",
                "consent": [consent],
                "key": "key-1",
            })
        );
        let error = tool.execute(context, json!({})).await.unwrap_err();
        assert_eq!(error.code, "awp.capability.refused");
        let body = adk_error_to_body(&error);
        assert_eq!(body.code, ErrorCode::SchemaValidationFailed);
        assert!(!body.retryable);
    }

    #[test]
    fn handler_failures_hide_their_detail() {
        let error = capability_error_to_adk(&CapabilityError::Failed("db password wrong".into()));
        let body = adk_error_to_body(&error);
        assert_eq!(body.code, ErrorCode::InternalError);
        assert!(!body.message.contains("password"));
    }
}
