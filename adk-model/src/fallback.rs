//! A provider-neutral fallback chain: an ordered list of models, each tried when the one
//! before it fails.
//!
//! [`FallbackLlm`] is an [`Llm`], so an agent takes it wherever it takes any other model. It
//! sends the request to its first model and moves to the next one only on a configured
//! [`FallbackTrigger`]; the first model that answers keeps the turn.
//!
//! | Step | What happens |
//! |------|--------------|
//! | Each attempt | The request goes out with `model` set to that model's name |
//! | The attempt fails before its first response | On a configured trigger the next model is tried; otherwise the error is returned |
//! | The attempt's first response is a refusal | With [`FallbackTrigger::Refusal`] the next model is tried |
//! | The attempt has yielded a response | The turn belongs to that model: a later error reaches the caller |
//! | Every model failed | The last model's error, or its refusal, is returned |
//!
//! Under a [`ModelCallScope`] (an agent's run) every attempt is its own metered call: it
//! counts against the run budget as a model call and is reserved and committed in the
//! spend ledger under the provider and model that served it, a refused attempt included.

use std::sync::Arc;

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, FinishReason, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, ModelCallScope, NativeWebSearch, Result, SchemaAdapter,
    intra_compaction::is_context_limit_error,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

/// The `provider_metadata` key a [`FallbackLlm`] response carries when a model after the
/// first one answered.
///
/// Its value names the model that answered, its position in the chain (1 is the first
/// fallback), and each model skipped before it with the trigger and error code that moved the
/// chain on.
///
/// # Example
///
/// ```rust
/// use adk_model::fallback::FALLBACK_METADATA_KEY;
///
/// assert_eq!(FALLBACK_METADATA_KEY, "fallback");
/// ```
pub const FALLBACK_METADATA_KEY: &str = "fallback";

/// A condition that moves a [`FallbackLlm`] on to its next model.
///
/// Only a failure the model reports counts: an error from the run's budget, spend ledger or
/// kill switch is never a reason to try another model, and is returned as is.
///
/// | Trigger | Default | Matches |
/// |---------|---------|---------|
/// | [`Retryable`](Self::Retryable) | on | Rate limiting, timeouts, server errors and provider outages ([`AdkError::is_retryable`]), once the model's own retries are used up |
/// | [`ModelNotFound`](Self::ModelNotFound) | on | The provider does not serve the model: an unknown, retired or unavailable model id ([`ErrorCategory::NotFound`]) |
/// | [`QuotaExhausted`](Self::QuotaExhausted) | on | The provider account's quota, credit balance or spend limit is used up ([`ErrorCategory::ResourceExhausted`]) |
/// | [`Refusal`](Self::Refusal) | off | The model declined the request ([`FinishReason::Refusal`]) before any output |
/// | [`ContextOverflow`](Self::ContextOverflow) | off | The request is longer than the model's context window |
/// | [`AuthFailure`](Self::AuthFailure) | off | The provider refused the credentials or the permission ([`ErrorCategory::Unauthorized`], [`ErrorCategory::Forbidden`]) |
///
/// # Example
///
/// ```rust
/// use adk_model::fallback::FallbackTrigger;
///
/// assert!(FallbackTrigger::DEFAULT.contains(&FallbackTrigger::Retryable));
/// assert!(!FallbackTrigger::DEFAULT.contains(&FallbackTrigger::Refusal));
/// assert_eq!(FallbackTrigger::ModelNotFound.as_str(), "model_not_found");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FallbackTrigger {
    /// A transient failure the model's own retries did not clear.
    Retryable,
    /// The provider does not serve the requested model.
    ModelNotFound,
    /// The provider account's quota or credit is used up.
    QuotaExhausted,
    /// The model declined the request before producing any output.
    Refusal,
    /// The request does not fit the model's context window.
    ContextOverflow,
    /// The provider refused the credentials or the permission.
    AuthFailure,
}

impl FallbackTrigger {
    /// The triggers a [`FallbackLlm`] uses unless configured otherwise: the failures another
    /// model can serve without a change to the request or to its permissions.
    pub const DEFAULT: [FallbackTrigger; 3] = [
        FallbackTrigger::Retryable,
        FallbackTrigger::ModelNotFound,
        FallbackTrigger::QuotaExhausted,
    ];

    /// The trigger's name in tracing and in [`FALLBACK_METADATA_KEY`] metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackTrigger::Retryable => "retryable",
            FallbackTrigger::ModelNotFound => "model_not_found",
            FallbackTrigger::QuotaExhausted => "quota_exhausted",
            FallbackTrigger::Refusal => "refusal",
            FallbackTrigger::ContextOverflow => "context_overflow",
            FallbackTrigger::AuthFailure => "auth_failure",
        }
    }

    /// The trigger `error` matches, if any.
    ///
    /// Only an error from the model ([`ErrorComponent::Model`]) matches, so a refusal by the
    /// run's budget, spend ledger or kill switch never moves a chain on.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{AdkError, ErrorCategory, ErrorComponent};
    /// use adk_model::fallback::FallbackTrigger;
    ///
    /// let overloaded = AdkError::new(
    ///     ErrorComponent::Model,
    ///     ErrorCategory::Unavailable,
    ///     "model.anthropic.api_error",
    ///     "overloaded",
    /// );
    /// assert_eq!(FallbackTrigger::for_error(&overloaded), Some(FallbackTrigger::Retryable));
    ///
    /// let budget = AdkError::new(
    ///     ErrorComponent::Agent,
    ///     ErrorCategory::ResourceExhausted,
    ///     "budget.model_calls_exceeded",
    ///     "the run's model-call budget is spent",
    /// );
    /// assert_eq!(FallbackTrigger::for_error(&budget), None);
    /// ```
    pub fn for_error(error: &AdkError) -> Option<Self> {
        if error.component != ErrorComponent::Model {
            return None;
        }
        if is_context_limit_error(error) {
            return Some(FallbackTrigger::ContextOverflow);
        }
        match error.category {
            ErrorCategory::RateLimited | ErrorCategory::Timeout | ErrorCategory::Unavailable => {
                Some(FallbackTrigger::Retryable)
            }
            ErrorCategory::NotFound => Some(FallbackTrigger::ModelNotFound),
            ErrorCategory::ResourceExhausted => Some(FallbackTrigger::QuotaExhausted),
            ErrorCategory::Unauthorized | ErrorCategory::Forbidden => {
                Some(FallbackTrigger::AuthFailure)
            }
            ErrorCategory::InvalidInput
            | ErrorCategory::Cancelled
            | ErrorCategory::Internal
            | ErrorCategory::Unsupported
            | ErrorCategory::Conflict => error.is_retryable().then_some(FallbackTrigger::Retryable),
        }
    }
}

impl std::fmt::Display for FallbackTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An ordered chain of models: each request goes to the first model, and to the next one
/// whenever the one before it fails on a configured [`FallbackTrigger`].
///
/// The counterpart of Google ADK's `FallbackModel`, for any mix of providers. The first
/// model that yields a response keeps the turn, so a streamed turn never splices two
/// models' output: a failure after the first response reaches the caller. When every model
/// fails, the last model's error (or refusal) is returned, and `LlmAgent`'s
/// `on_model_error` hooks see it as they see any model error.
///
/// | Signal | Where |
/// |--------|-------|
/// | Which model answered | [`LlmResponse::model`] and [`LlmResponse::provider`], set by the provider that answered |
/// | That a fallback happened | `provider_metadata["fallback"]` ([`FALLBACK_METADATA_KEY`]) on every complete response, and a `warn` tracing event per skipped model |
/// | What each attempt cost | Under a run, one spend-ledger entry and one budget model call per attempt, under the attempt's own provider |
///
/// For a mix of models, the chain reports:
///
/// - [`name`](Llm::name): the first model's name.
/// - [`context_window`](Llm::context_window): the smallest window among the models that
///   report one, so auto-compaction keeps a request within every model it may reach; with
///   [`FallbackTrigger::ContextOverflow`], the largest, since the chain is then built to move
///   a long request to a larger model.
/// - [`schema_adapter`](Llm::schema_adapter): the first model's. Tool schemas are normalized
///   once, before any attempt, so keep the chain's providers schema-compatible or put the
///   strictest one first.
/// - [`max_cost_micro_usd`](Llm::max_cost_micro_usd): the largest bound in the chain (a
///   failed call bills nothing), or the sum of the bounds with [`FallbackTrigger::Refusal`],
///   since a refused attempt is billed; `None` when a model has no bound. A run meters each
///   attempt on its own bound, so this applies only where the chain is metered as one call.
/// - [`native_web_search`](Llm::native_web_search): the search every model serves, or `None`
///   when they differ, since a provider's built-in search tool breaks another provider's
///   request.
///
/// A model's own retries ([`RetryConfig`](crate::RetryConfig)) run first: the chain sees a
/// failure only once they are used up, and it tries each model once.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Llm, LlmRequest};
/// use adk_model::fallback::{FallbackLlm, FallbackTrigger};
/// use adk_model::{ScriptedLlm, ScriptedResponse};
/// use futures::StreamExt;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), AdkError> {
/// let overloaded = AdkError::new(
///     ErrorComponent::Model,
///     ErrorCategory::Unavailable,
///     "model.anthropic.api_error",
///     "overloaded",
/// );
/// let primary = Arc::new(ScriptedLlm::new("claude-sonnet-5-5", [ScriptedResponse::error(overloaded)]));
/// let backup = Arc::new(ScriptedLlm::new("gemini-3.7-flash", [ScriptedResponse::text("Mars.")]));
///
/// let model = FallbackLlm::builder(primary)
///     .fallback(backup.clone())
///     .fall_back_on(FallbackTrigger::Refusal)
///     .build();
///
/// let request = LlmRequest::new(model.name(), vec![Content::new("user").with_text("Name a planet.")]);
/// let response = model.generate_content(request, false).await?.next().await.unwrap()?;
/// assert_eq!(response.content.unwrap().parts[0].text(), Some("Mars."));
/// assert_eq!(response.provider_metadata.unwrap()["fallback"]["model"], "gemini-3.7-flash");
/// assert_eq!(backup.last_request().unwrap().model, "gemini-3.7-flash");
/// # Ok(())
/// # }
/// ```
pub struct FallbackLlm {
    models: Vec<Arc<dyn Llm>>,
    triggers: Vec<FallbackTrigger>,
}

impl std::fmt::Debug for FallbackLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackLlm")
            .field("models", &self.models.iter().map(|model| model.name()).collect::<Vec<_>>())
            .field("triggers", &self.triggers)
            .finish()
    }
}

/// Builds a [`FallbackLlm`]: the first model, the models to fall back to in order, and the
/// triggers that move the chain on.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::Llm;
/// use adk_model::fallback::{FallbackLlm, FallbackTrigger};
/// use adk_model::{ScriptedLlm, ScriptedResponse};
///
/// let primary = Arc::new(ScriptedLlm::new("claude-sonnet-5-5", Vec::<ScriptedResponse>::new()));
/// let backup = Arc::new(ScriptedLlm::new("claude-haiku-5-5", Vec::<ScriptedResponse>::new()));
/// let model = FallbackLlm::builder(primary)
///     .fallback(backup)
///     .never_fall_back_on(FallbackTrigger::ModelNotFound)
///     .build();
/// assert_eq!(model.name(), "claude-sonnet-5-5");
/// assert_eq!(model.triggers(), [FallbackTrigger::Retryable, FallbackTrigger::QuotaExhausted]);
/// ```
pub struct FallbackLlmBuilder {
    models: Vec<Arc<dyn Llm>>,
    triggers: Vec<FallbackTrigger>,
}

impl FallbackLlm {
    /// Starts a chain whose first model is `primary`, with the [default
    /// triggers](FallbackTrigger::DEFAULT).
    pub fn builder(primary: Arc<dyn Llm>) -> FallbackLlmBuilder {
        FallbackLlmBuilder { models: vec![primary], triggers: FallbackTrigger::DEFAULT.to_vec() }
    }

    /// The models of the chain, first model first.
    pub fn models(&self) -> &[Arc<dyn Llm>] {
        &self.models
    }

    /// The triggers that move the chain to its next model.
    pub fn triggers(&self) -> &[FallbackTrigger] {
        &self.triggers
    }

    fn falls_back_on(&self, trigger: FallbackTrigger) -> bool {
        self.triggers.contains(&trigger)
    }

    /// Tries each model in turn, metering every attempt against `scope` when there is one.
    async fn generate(
        &self,
        scope: Option<&ModelCallScope>,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        let mut skipped: Vec<Value> = Vec::new();
        let mut models = self.models.iter().enumerate().peekable();
        while let Some((position, model)) = models.next() {
            let has_next = models.peek().is_some();
            let mut attempt = request.clone();
            attempt.model = model.name().to_string();
            let started = match scope {
                Some(scope) => scope.generate(model.as_ref(), attempt, stream).await,
                None => model.generate_content(attempt, stream).await,
            };
            let mut responses = match started {
                Ok(responses) => responses,
                Err(error) => {
                    let trigger =
                        FallbackTrigger::for_error(&error).filter(|t| self.falls_back_on(*t));
                    match trigger {
                        Some(trigger) if has_next => {
                            skipped.push(self.skip(model.as_ref(), trigger, Some(error.code)));
                            continue;
                        }
                        _ => return Err(error),
                    }
                }
            };
            let first = responses.next().await;
            let trigger = match &first {
                Some(Err(error)) => {
                    FallbackTrigger::for_error(error).filter(|t| self.falls_back_on(*t))
                }
                Some(Ok(response)) => (response.finish_reason == Some(FinishReason::Refusal)
                    && self.falls_back_on(FallbackTrigger::Refusal))
                .then_some(FallbackTrigger::Refusal),
                None => None,
            };
            if let Some(trigger) = trigger
                && has_next
            {
                let code = match &first {
                    Some(Err(error)) => Some(error.code),
                    _ => None,
                };
                // Read to the end so the attempt's spend settles — a refusal is billed —
                // before the next model reserves.
                while responses.next().await.is_some() {}
                skipped.push(self.skip(model.as_ref(), trigger, code));
                continue;
            }
            let responses: LlmResponseStream = match first {
                Some(first) => Box::pin(futures::stream::iter([first]).chain(responses)),
                None => responses,
            };
            if skipped.is_empty() {
                return Ok(responses);
            }
            tracing::info!(
                fallback.model = %model.name(),
                fallback.position = position,
                "fallback model answered"
            );
            let marker = json!({
                "model": model.name(),
                "position": position,
                "skipped": skipped,
            });
            return Ok(Box::pin(responses.map(move |item| {
                item.map(|mut response| {
                    if !response.partial {
                        mark(&mut response, &marker);
                    }
                    response
                })
            })));
        }
        Err(AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "model.fallback.empty",
            "the fallback chain has no model; build it with FallbackLlm::builder",
        ))
    }

    /// Records one skipped model: a `warn` event and its entry in the response metadata.
    fn skip(&self, model: &dyn Llm, trigger: FallbackTrigger, code: Option<&str>) -> Value {
        tracing::warn!(
            fallback.from = %model.name(),
            fallback.reason = trigger.as_str(),
            error.code = code.unwrap_or(""),
            "model failed; trying the next model in the fallback chain"
        );
        let mut entry = json!({ "model": model.name(), "reason": trigger.as_str() });
        if let Some(code) = code {
            entry["error_code"] = json!(code);
        }
        entry
    }
}

/// Adds the fallback marker to `response`'s provider metadata, keeping what the provider set.
fn mark(response: &mut LlmResponse, marker: &Value) {
    match response.provider_metadata.get_or_insert_with(|| json!({})) {
        Value::Object(fields) => {
            fields.insert(FALLBACK_METADATA_KEY.to_string(), marker.clone());
        }
        other => {
            let original = std::mem::take(other);
            *other = json!({ "provider": original, FALLBACK_METADATA_KEY: marker.clone() });
        }
    }
}

impl FallbackLlmBuilder {
    /// Appends `model` to the chain, after the models already in it.
    pub fn fallback(mut self, model: Arc<dyn Llm>) -> Self {
        self.models.push(model);
        self
    }

    /// Moves the chain on when `trigger` matches.
    pub fn fall_back_on(mut self, trigger: FallbackTrigger) -> Self {
        if !self.triggers.contains(&trigger) {
            self.triggers.push(trigger);
        }
        self
    }

    /// Returns the error, or the refusal, when `trigger` matches, instead of moving on.
    pub fn never_fall_back_on(mut self, trigger: FallbackTrigger) -> Self {
        self.triggers.retain(|existing| *existing != trigger);
        self
    }

    /// Replaces the triggers with `triggers`.
    pub fn triggers(mut self, triggers: impl IntoIterator<Item = FallbackTrigger>) -> Self {
        self.triggers.clear();
        for trigger in triggers {
            self = self.fall_back_on(trigger);
        }
        self
    }

    /// Builds the chain.
    pub fn build(self) -> FallbackLlm {
        FallbackLlm { models: self.models, triggers: self.triggers }
    }
}

#[async_trait]
impl Llm for FallbackLlm {
    fn name(&self) -> &str {
        self.models.first().map_or("fallback", |model| model.name())
    }

    async fn generate_content(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        self.generate(None, req, stream).await
    }

    async fn generate_content_metered(
        &self,
        scope: &ModelCallScope,
        req: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        self.generate(Some(scope), req, stream).await
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        match self.models.first() {
            Some(model) => model.schema_adapter(),
            None => &adk_core::GenericSchemaAdapter,
        }
    }

    fn uses_interactions_api(&self) -> bool {
        self.models.iter().any(|model| model.uses_interactions_api())
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        let bounds = self
            .models
            .iter()
            .map(|model| {
                let mut attempt = request.clone();
                attempt.model = model.name().to_string();
                model.max_cost_micro_usd(&attempt)
            })
            .collect::<Option<Vec<u64>>>()?;
        if self.falls_back_on(FallbackTrigger::Refusal) {
            Some(bounds.into_iter().fold(0u64, u64::saturating_add))
        } else {
            bounds.into_iter().max()
        }
    }

    fn context_window(&self) -> Option<u64> {
        let windows = self.models.iter().filter_map(|model| model.context_window());
        if self.falls_back_on(FallbackTrigger::ContextOverflow) {
            windows.max()
        } else {
            windows.min()
        }
    }

    fn native_web_search(&self) -> Option<NativeWebSearch> {
        let mut searches = self.models.iter().map(|model| model.native_web_search());
        let first = searches.next()??;
        searches.all(|search| search == Some(first)).then_some(first)
    }

    // A compaction request may fall back to any model in the chain, so it is native only when
    // every model compacts the same way.
    fn native_compaction(&self) -> Option<adk_core::NativeCompaction> {
        let mut compactions = self.models.iter().map(|model| model.native_compaction());
        let first = compactions.next()??;
        compactions.all(|compaction| compaction == Some(first)).then_some(first)
    }

    /// Prewarms the first model, which serves the next call; the fallbacks are prewarmed too,
    /// and their failures only logged.
    async fn prewarm(&self) -> Result<()> {
        let mut models = self.models.iter();
        let Some(first) = models.next() else { return Ok(()) };
        for model in models {
            if let Err(error) = model.prewarm().await {
                tracing::debug!(model = model.name(), error = %error, "a fallback model did not prewarm");
            }
        }
        first.prewarm().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScriptedLlm, ScriptedResponse};
    use adk_core::{
        BudgetTracker, Content, GovernanceControl, InMemorySpendLedger, NativeWebSearchTool,
        RunBudget, SpendKey, SpendLedger, SpendLimits, SpendPeriod, UsageMetadata,
    };

    fn error(category: ErrorCategory, code: &'static str) -> AdkError {
        AdkError::new(ErrorComponent::Model, category, code, format!("{code} failure"))
    }

    fn unavailable() -> AdkError {
        error(ErrorCategory::Unavailable, "model.test.unavailable")
    }

    /// A complete answer `text` from `model` at `provider`, reporting `cost` in USD.
    fn answer(text: &str, model: &str, provider: &str, cost: f64) -> ScriptedResponse {
        let mut response = LlmResponse::new(Content::new("model").with_text(text));
        response.model = Some(model.to_string());
        response.provider = Some(provider.to_string());
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 5,
            total_token_count: 15,
            cost: Some(cost),
            ..Default::default()
        });
        ScriptedResponse::response(response)
    }

    /// A refusal before any output, as Anthropic's classifiers return one.
    fn refusal(model: &str, provider: &str, cost: f64) -> ScriptedResponse {
        let response = LlmResponse {
            finish_reason: Some(FinishReason::Refusal),
            turn_complete: true,
            model: Some(model.to_string()),
            provider: Some(provider.to_string()),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 10,
                total_token_count: 10,
                cost: Some(cost),
                ..Default::default()
            }),
            ..Default::default()
        };
        ScriptedResponse::response(response)
    }

    fn partial(text: &str) -> LlmResponse {
        let mut response = LlmResponse::new(Content::new("model").with_text(text));
        response.partial = true;
        response.turn_complete = false;
        response.finish_reason = None;
        response
    }

    fn scripted<R: Into<ScriptedResponse>>(
        name: &str,
        script: impl IntoIterator<Item = R>,
    ) -> Arc<ScriptedLlm> {
        Arc::new(ScriptedLlm::new(name, script))
    }

    fn request() -> LlmRequest {
        LlmRequest::new("claude-sonnet-5-5", vec![Content::new("user").with_text("hello")])
    }

    async fn collect(model: &FallbackLlm, stream: bool) -> Vec<Result<LlmResponse>> {
        model.generate_content(request(), stream).await.unwrap().collect().await
    }

    fn text(response: &LlmResponse) -> String {
        response.content.iter().flat_map(|c| c.parts.iter()).filter_map(|p| p.text()).collect()
    }

    #[tokio::test]
    async fn a_retryable_error_moves_to_the_next_model() {
        let primary = scripted("claude-sonnet-5-5", [ScriptedResponse::error(unavailable())]);
        let backup =
            scripted("gemini-3.7-flash", [answer("hi", "gemini-3.7-flash", "gemini", 0.0)]);
        let model = FallbackLlm::builder(primary.clone()).fallback(backup.clone()).build();

        let responses = collect(&model, false).await;
        let response = responses[0].as_ref().unwrap();
        assert_eq!(text(response), "hi");
        assert_eq!(response.model.as_deref(), Some("gemini-3.7-flash"));
        assert_eq!(
            response.provider_metadata.as_ref().unwrap()[FALLBACK_METADATA_KEY],
            json!({
                "model": "gemini-3.7-flash",
                "position": 1,
                "skipped": [{
                    "model": "claude-sonnet-5-5",
                    "reason": "retryable",
                    "error_code": "model.test.unavailable",
                }],
            })
        );
        assert_eq!(primary.last_request().unwrap().model, "claude-sonnet-5-5");
        assert_eq!(backup.last_request().unwrap().model, "gemini-3.7-flash");
    }

    #[tokio::test]
    async fn rate_limits_timeouts_unknown_models_and_spent_quotas_fall_back_by_default() {
        for (category, reason) in [
            (ErrorCategory::RateLimited, "retryable"),
            (ErrorCategory::Timeout, "retryable"),
            (ErrorCategory::NotFound, "model_not_found"),
            (ErrorCategory::ResourceExhausted, "quota_exhausted"),
        ] {
            let primary =
                scripted("claude-sonnet-5-5", [ScriptedResponse::error(error(category, "x"))]);
            let backup = scripted("claude-haiku-5-5", [ScriptedResponse::text("ok")]);
            let model = FallbackLlm::builder(primary).fallback(backup).build();
            let responses = collect(&model, false).await;
            let marker = &responses[0].as_ref().unwrap().provider_metadata.as_ref().unwrap()
                [FALLBACK_METADATA_KEY];
            assert_eq!(marker["skipped"][0]["reason"], reason, "{category:?}");
        }
    }

    #[tokio::test]
    async fn an_error_at_the_start_of_the_stream_falls_back() {
        let primary = scripted(
            "claude-sonnet-5-5",
            [ScriptedResponse::stream(Vec::<LlmResponse>::new()).then_error(unavailable())],
        );
        let backup = scripted("claude-haiku-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary).fallback(backup).build();

        let responses = collect(&model, true).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(text(responses[0].as_ref().unwrap()), "ok");
    }

    #[tokio::test]
    async fn auth_and_permission_errors_are_returned_unless_configured() {
        for category in [ErrorCategory::Unauthorized, ErrorCategory::Forbidden] {
            let primary =
                scripted("claude-sonnet-5-5", [ScriptedResponse::error(error(category, "auth"))]);
            let backup = scripted("claude-haiku-5-5", [ScriptedResponse::text("ok")]);
            let model = FallbackLlm::builder(primary).fallback(backup.clone()).build();
            let failed = model.generate_content(request(), false).await.err().unwrap();
            assert_eq!(failed.category, category);
            assert!(backup.requests().is_empty());

            let primary =
                scripted("claude-sonnet-5-5", [ScriptedResponse::error(error(category, "auth"))]);
            let backup = scripted("claude-haiku-5-5", [ScriptedResponse::text("ok")]);
            let model = FallbackLlm::builder(primary)
                .fallback(backup)
                .fall_back_on(FallbackTrigger::AuthFailure)
                .build();
            assert_eq!(text(collect(&model, false).await[0].as_ref().unwrap()), "ok");
        }
    }

    #[tokio::test]
    async fn a_refusal_falls_back_only_when_configured() {
        let primary =
            scripted("claude-sonnet-5-5", [refusal("claude-sonnet-5-5", "anthropic", 0.0)]);
        let backup = scripted("claude-opus-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary).fallback(backup.clone()).build();
        let responses = collect(&model, false).await;
        assert_eq!(responses[0].as_ref().unwrap().finish_reason, Some(FinishReason::Refusal));
        assert!(backup.requests().is_empty());

        let primary =
            scripted("claude-sonnet-5-5", [refusal("claude-sonnet-5-5", "anthropic", 0.0)]);
        let backup = scripted("claude-opus-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary)
            .fallback(backup)
            .fall_back_on(FallbackTrigger::Refusal)
            .build();
        let responses = collect(&model, false).await;
        let response = responses[0].as_ref().unwrap();
        assert_eq!(text(response), "ok");
        assert_eq!(
            response.provider_metadata.as_ref().unwrap()[FALLBACK_METADATA_KEY]["skipped"][0],
            json!({ "model": "claude-sonnet-5-5", "reason": "refusal" })
        );
    }

    #[tokio::test]
    async fn context_overflow_falls_back_only_when_configured() {
        let invalid = || {
            ScriptedResponse::error(error(ErrorCategory::InvalidInput, "model.anthropic.api_error"))
        };
        let overflow = || {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.anthropic.api_error",
                "400: prompt is too long: 1048577 tokens > 1000000 maximum",
            )
        };
        assert_eq!(FallbackTrigger::for_error(&overflow()), Some(FallbackTrigger::ContextOverflow));
        // An invalid request that is not about length never falls back.
        let primary = scripted("claude-haiku-4-5", [invalid()]);
        let backup = scripted("claude-sonnet-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary)
            .fallback(backup.clone())
            .fall_back_on(FallbackTrigger::ContextOverflow)
            .build();
        assert!(model.generate_content(request(), false).await.is_err());
        assert!(backup.requests().is_empty());

        let primary = scripted("claude-haiku-4-5", [ScriptedResponse::error(overflow())]);
        let backup = scripted("claude-sonnet-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary).fallback(backup.clone()).build();
        assert!(model.generate_content(request(), false).await.is_err());
        assert!(backup.requests().is_empty());

        let primary = scripted("claude-haiku-4-5", [ScriptedResponse::error(overflow())]);
        let backup = scripted("claude-sonnet-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary)
            .fallback(backup)
            .fall_back_on(FallbackTrigger::ContextOverflow)
            .build();
        assert_eq!(text(collect(&model, false).await[0].as_ref().unwrap()), "ok");
    }

    #[tokio::test]
    async fn after_the_first_chunk_an_error_reaches_the_caller() {
        let primary = scripted(
            "claude-sonnet-5-5",
            [ScriptedResponse::stream([partial("Hel")]).then_error(unavailable())],
        );
        let backup = scripted("claude-haiku-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary).fallback(backup.clone()).build();

        let responses = collect(&model, true).await;
        assert_eq!(text(responses[0].as_ref().unwrap()), "Hel");
        assert_eq!(responses[1].as_ref().unwrap_err().code, "model.test.unavailable");
        assert!(backup.requests().is_empty());
    }

    #[tokio::test]
    async fn a_refusal_after_streamed_output_reaches_the_caller() {
        let refused = LlmResponse {
            finish_reason: Some(FinishReason::Refusal),
            turn_complete: true,
            ..Default::default()
        };
        let primary =
            scripted("claude-sonnet-5-5", [ScriptedResponse::stream([partial("Sure"), refused])]);
        let backup = scripted("claude-opus-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary)
            .fallback(backup.clone())
            .fall_back_on(FallbackTrigger::Refusal)
            .build();

        let responses = collect(&model, true).await;
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[1].as_ref().unwrap().finish_reason, Some(FinishReason::Refusal));
        assert!(backup.requests().is_empty());
    }

    #[tokio::test]
    async fn when_every_model_fails_the_last_error_is_returned() {
        let primary = scripted("claude-sonnet-5-5", [ScriptedResponse::error(unavailable())]);
        let backup = scripted(
            "claude-haiku-5-5",
            [ScriptedResponse::error(error(ErrorCategory::RateLimited, "model.test.limited"))],
        );
        let model = FallbackLlm::builder(primary).fallback(backup).build();
        let failed = model.generate_content(request(), false).await.err().unwrap();
        assert_eq!(failed.code, "model.test.limited");

        // The last model's refusal is its answer.
        let primary = scripted("claude-sonnet-5-5", [ScriptedResponse::error(unavailable())]);
        let backup = scripted("claude-opus-5-5", [refusal("claude-opus-5-5", "anthropic", 0.0)]);
        let model = FallbackLlm::builder(primary)
            .fallback(backup)
            .fall_back_on(FallbackTrigger::Refusal)
            .build();
        let responses = collect(&model, false).await;
        assert_eq!(responses[0].as_ref().unwrap().finish_reason, Some(FinishReason::Refusal));
    }

    #[tokio::test]
    async fn a_model_that_answers_first_time_is_not_marked() {
        let primary = scripted("claude-sonnet-5-5", [ScriptedResponse::text("ok")]);
        let backup = scripted("claude-haiku-5-5", Vec::<ScriptedResponse>::new());
        let model = FallbackLlm::builder(primary).fallback(backup).build();
        let responses = collect(&model, false).await;
        assert!(responses[0].as_ref().unwrap().provider_metadata.is_none());
    }

    fn ledger() -> Arc<InMemorySpendLedger> {
        Arc::new(InMemorySpendLedger::new(SpendLimits::new()))
    }

    async fn spent(ledger: &InMemorySpendLedger, vendor: &str) -> u64 {
        ledger.spent(&SpendKey::org("acme").with_vendor(vendor), SpendPeriod::Day).await.unwrap()
    }

    #[tokio::test]
    async fn every_attempt_is_metered_under_its_own_provider() {
        let ledger = ledger();
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
        let scope = ModelCallScope::new()
            .with_budget(tracker.clone())
            .with_spend_ledger(ledger.clone(), "acme");
        // A billed refusal on OpenAI, an outage on Gemini, the answer on Anthropic.
        let refused = scripted("gpt-6.1-sol", [refusal("gpt-6.1-sol", "openai", 0.001)]);
        let down = scripted("gemini-3.7-flash", [ScriptedResponse::error(unavailable())]);
        let answers =
            scripted("claude-haiku-5-5", [answer("ok", "claude-haiku-5-5", "anthropic", 0.002)]);
        let model = FallbackLlm::builder(refused)
            .fallback(down)
            .fallback(answers)
            .fall_back_on(FallbackTrigger::Refusal)
            .build();

        let mut responses = scope.generate(&model, request(), false).await.unwrap();
        let response = responses.next().await.unwrap().unwrap();
        assert!(responses.next().await.is_none());
        assert_eq!(response.model.as_deref(), Some("claude-haiku-5-5"));

        assert_eq!(spent(&ledger, "openai").await, 1_000);
        assert_eq!(spent(&ledger, "gemini").await, 0);
        assert_eq!(spent(&ledger, "anthropic").await, 2_000);
        assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 3_000);
        let usage = tracker.usage();
        assert_eq!((usage.model_calls, usage.cost_micro_usd, usage.total_tokens), (3, 3_000, 25));
    }

    #[tokio::test]
    async fn a_budget_refusal_ends_the_chain_without_falling_back() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(1)));
        let scope = ModelCallScope::new().with_budget(tracker.clone());
        let primary = scripted("claude-sonnet-5-5", [ScriptedResponse::error(unavailable())]);
        let backup = scripted("claude-haiku-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary).fallback(backup.clone()).build();

        let refused = scope.generate(&model, request(), false).await.err().unwrap();
        assert_eq!(refused.category, ErrorCategory::ResourceExhausted);
        assert!(backup.requests().is_empty());
        assert_eq!(tracker.usage().model_calls, 1);
    }

    #[tokio::test]
    async fn a_frozen_kill_switch_stops_the_chain_before_the_next_model() {
        let control = GovernanceControl::new();
        let scope = ModelCallScope::new().with_governance(control.clone());
        let freezes = control.clone();
        // The first model's failure is the moment an operator freezes the run.
        struct FreezesOnCall {
            inner: Arc<ScriptedLlm>,
            control: GovernanceControl,
        }
        #[async_trait]
        impl Llm for FreezesOnCall {
            fn name(&self) -> &str {
                self.inner.name()
            }
            async fn generate_content(
                &self,
                req: LlmRequest,
                stream: bool,
            ) -> Result<LlmResponseStream> {
                self.control.freeze("incident");
                self.inner.generate_content(req, stream).await
            }
        }
        let primary = Arc::new(FreezesOnCall {
            inner: scripted("claude-sonnet-5-5", [ScriptedResponse::error(unavailable())]),
            control: freezes,
        });
        let backup = scripted("claude-haiku-5-5", [ScriptedResponse::text("ok")]);
        let model = FallbackLlm::builder(primary).fallback(backup.clone()).build();

        let refused = scope.generate(&model, request(), false).await.err().unwrap();
        assert_eq!(refused.code, adk_core::GOVERNANCE_FROZEN_CODE);
        assert!(backup.requests().is_empty());
    }

    struct Described {
        name: &'static str,
        window: Option<u64>,
        bound: Option<u64>,
        search: Option<NativeWebSearch>,
    }

    #[async_trait]
    impl Llm for Described {
        fn name(&self) -> &str {
            self.name
        }
        async fn generate_content(&self, _: LlmRequest, _: bool) -> Result<LlmResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
        fn context_window(&self) -> Option<u64> {
            self.window
        }
        fn max_cost_micro_usd(&self, _: &LlmRequest) -> Option<u64> {
            self.bound
        }
        fn native_web_search(&self) -> Option<NativeWebSearch> {
            self.search
        }
    }

    #[test]
    fn a_mixed_chain_reports_the_limits_every_model_can_serve() {
        let search =
            NativeWebSearch::new("anthropic", NativeWebSearchTool::AnthropicWebSearch, true);
        let small = Arc::new(Described {
            name: "claude-haiku-4-5",
            window: Some(200_000),
            bound: Some(10_000),
            search: Some(search),
        });
        let large = Arc::new(Described {
            name: "claude-sonnet-5-5",
            window: Some(1_000_000),
            bound: Some(40_000),
            search: Some(search),
        });
        let unknown =
            Arc::new(Described { name: "local", window: None, bound: None, search: None });

        let model = FallbackLlm::builder(small.clone()).fallback(large.clone()).build();
        assert_eq!(model.name(), "claude-haiku-4-5");
        assert_eq!(model.context_window(), Some(200_000));
        assert_eq!(model.max_cost_micro_usd(&request()), Some(40_000));
        assert_eq!(model.native_web_search(), Some(search));

        let model = FallbackLlm::builder(small.clone())
            .fallback(large.clone())
            .fall_back_on(FallbackTrigger::ContextOverflow)
            .fall_back_on(FallbackTrigger::Refusal)
            .build();
        assert_eq!(model.context_window(), Some(1_000_000));
        assert_eq!(model.max_cost_micro_usd(&request()), Some(50_000));

        let model = FallbackLlm::builder(small).fallback(large).fallback(unknown).build();
        assert_eq!(model.context_window(), Some(200_000));
        assert_eq!(model.max_cost_micro_usd(&request()), None);
        assert_eq!(model.native_web_search(), None);
    }

    #[test]
    fn the_builder_edits_the_trigger_set() {
        let primary = scripted("claude-sonnet-5-5", Vec::<ScriptedResponse>::new());
        let model = FallbackLlm::builder(primary.clone())
            .triggers([FallbackTrigger::Refusal, FallbackTrigger::Refusal])
            .fall_back_on(FallbackTrigger::Retryable)
            .build();
        assert_eq!(model.triggers(), [FallbackTrigger::Refusal, FallbackTrigger::Retryable]);
        assert_eq!(model.models().len(), 1);
        assert_eq!(
            format!("{model:?}"),
            "FallbackLlm { models: [\"claude-sonnet-5-5\"], triggers: [Refusal, Retryable] }"
        );
    }
}
