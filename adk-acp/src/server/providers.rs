//! Configurable LLM providers (`providers/list`, `providers/set` and
//! `providers/disable`).
//!
//! This is ACP's unstable configurable-provider surface
//! (`unstable_llm_providers`). An agent that advertises the `providers`
//! capability lets its client discover the model providers it can route to
//! and point each one at another API protocol, base URL and header set — for
//! example a company gateway — before it creates sessions. The configuration
//! lasts for the agent process and is not persisted.
//!
//! The server advertises `providers` only when an [`AcpProviders`] hook is
//! configured with
//! [`AcpServerConfigBuilder::providers`](super::AcpServerConfigBuilder::providers).
//! Before calling the hook, the server checks each request against the hook's
//! own [`list`](AcpProviders::list):
//!
//! - `providers/set` must name a listed provider and one of its supported
//!   protocols, with a non-empty base URL;
//! - `providers/disable` of an unlisted provider succeeds without calling the
//!   hook, and of a required provider is refused.
//!
//! [`ModelProviders`] is a ready-made hook for ADK-Rust agents: each provider
//! is a [`SwitchableModel`] the agent uses as its model, and a factory turns a
//! client's configuration into the model that backs it from then on.
//!
//! Headers may carry credentials. `providers/list` never returns them, the
//! server never logs them, and [`ProviderSettings`] redacts them from its
//! `Debug` output.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use adk_core::{
    AdkError, Llm, LlmRequest, LlmResponseStream, NativeWebSearch, Result as AdkResult,
    SchemaAdapter,
};
use agent_client_protocol::v1::schema::{DisableProviderRequest, SetProviderRequest};
pub use agent_client_protocol::v1::schema::{
    LlmProtocol, ProviderCurrentConfig, ProviderId, ProviderInfo,
};
use async_trait::async_trait;
use serde_json::Value;

use super::error::AcpServerError;

/// A client's configuration for one provider, from `providers/set`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProviderSettings {
    /// The provider being configured.
    pub provider_id: ProviderId,
    /// The API protocol to speak to it.
    pub api_type: LlmProtocol,
    /// The base URL of its API.
    pub base_url: String,
    /// The complete set of headers to send with each request, which may hold
    /// credentials.
    pub headers: HashMap<String, String>,
}

impl ProviderSettings {
    /// Settings for `provider_id` with no headers.
    pub fn new(
        provider_id: impl Into<ProviderId>,
        api_type: LlmProtocol,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            api_type,
            base_url: base_url.into(),
            headers: HashMap::new(),
        }
    }

    /// Replace the headers.
    pub fn headers(mut self, headers: HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// The non-secret routing target `providers/list` reports.
    pub fn current(&self) -> ProviderCurrentConfig {
        ProviderCurrentConfig::new(self.api_type.clone(), self.base_url.clone())
    }

    fn from_request(request: SetProviderRequest) -> Self {
        Self {
            provider_id: request.provider_id,
            api_type: request.api_type,
            base_url: request.base_url,
            headers: request.headers,
        }
    }
}

impl std::fmt::Debug for ProviderSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let header_names: Vec<&str> = self.headers.keys().map(String::as_str).collect();
        formatter
            .debug_struct("ProviderSettings")
            .field("provider_id", &self.provider_id)
            .field("api_type", &self.api_type)
            .field("base_url", &self.base_url)
            .field("header_names", &header_names)
            .finish()
    }
}

/// Lets ACP clients list and configure an ADK-Rust agent's model providers.
///
/// The server validates requests against [`list`](Self::list) before calling
/// [`set`](Self::set) or [`disable`](Self::disable); see the
/// [module documentation](self).
#[async_trait]
pub trait AcpProviders: Send + Sync {
    /// The configurable providers: each one's supported protocols, whether it
    /// is required, and its current non-secret routing target (`None` while it
    /// is disabled). Never include headers.
    async fn list(&self) -> Result<Vec<ProviderInfo>, AcpServerError>;

    /// Replace one provider's protocol, base URL and complete header map.
    async fn set(&self, settings: ProviderSettings) -> Result<(), AcpServerError>;

    /// Disable a listed provider that is not required.
    async fn disable(&self, provider_id: &ProviderId) -> Result<(), AcpServerError>;
}

/// Check `providers/set` against the hook's listing, then apply it.
pub(crate) async fn set_provider(
    providers: &dyn AcpProviders,
    request: SetProviderRequest,
) -> Result<(), AcpServerError> {
    let listed = providers.list().await?;
    let Some(provider) = listed.iter().find(|provider| provider.provider_id == request.provider_id)
    else {
        return Err(AcpServerError::MalformedMessage(format!(
            "unknown provider: {}",
            request.provider_id
        )));
    };
    if !provider.supported.contains(&request.api_type) {
        return Err(AcpServerError::Unsupported(format!(
            "provider '{}' does not support the API protocol {:?}",
            request.provider_id, request.api_type
        )));
    }
    if request.base_url.trim().is_empty() {
        return Err(AcpServerError::MalformedMessage(format!(
            "provider '{}' needs a base URL",
            request.provider_id
        )));
    }
    providers.set(ProviderSettings::from_request(request)).await
}

/// Check `providers/disable` against the hook's listing, then apply it.
pub(crate) async fn disable_provider(
    providers: &dyn AcpProviders,
    request: DisableProviderRequest,
) -> Result<(), AcpServerError> {
    let listed = providers.list().await?;
    match listed.iter().find(|provider| provider.provider_id == request.provider_id) {
        // Disabling an unknown provider is idempotent.
        None => Ok(()),
        Some(provider) if provider.required => Err(AcpServerError::MalformedMessage(format!(
            "provider '{}' is required and cannot be disabled",
            request.provider_id
        ))),
        Some(_) => providers.disable(&request.provider_id).await,
    }
}

// ─── An ADK-Rust model whose provider a client can switch ───────────────────

/// An [`Llm`] whose backing model can be replaced while the agent runs.
///
/// Give it to an agent as its model and to a [`ModelProvider`]; a client's
/// `providers/set` then replaces the model the agent's next requests go to.
/// A request already running finishes on the model it started with. Clones
/// share the same backing model.
///
/// [`name`](Llm::name) stays the name of the model it was created with.
/// Every other [`Llm`] method, and the schema adapter, follows the current
/// model. While the provider is disabled, requests fail.
#[derive(Clone)]
pub struct SwitchableModel {
    name: Arc<str>,
    state: Arc<SwitchState>,
    adapter: CurrentAdapter,
}

struct SwitchState {
    current: RwLock<Option<Arc<dyn Llm>>>,
}

impl SwitchState {
    fn current(&self) -> Option<Arc<dyn Llm>> {
        self.current.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl SwitchableModel {
    /// A switchable model backed by `initial`.
    pub fn new(initial: Arc<dyn Llm>) -> Self {
        let state = Arc::new(SwitchState { current: RwLock::new(Some(initial.clone())) });
        Self { name: Arc::from(initial.name()), adapter: CurrentAdapter(state.clone()), state }
    }

    /// The model requests go to now, or `None` while disabled.
    pub fn current(&self) -> Option<Arc<dyn Llm>> {
        self.state.current()
    }

    /// Send the following requests to `model`.
    pub fn replace(&self, model: Arc<dyn Llm>) {
        *self.state.current.write().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(model);
    }

    /// Fail the following requests until [`replace`](Self::replace) is called.
    pub fn disable(&self) {
        *self.state.current.write().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn require(&self) -> AdkResult<Arc<dyn Llm>> {
        self.current().ok_or_else(|| {
            AdkError::model(format!("the model provider behind '{}' is disabled", self.name))
        })
    }
}

impl std::fmt::Debug for SwitchableModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SwitchableModel")
            .field("name", &self.name)
            .field("current", &self.current().map(|model| model.name().to_string()))
            .finish()
    }
}

#[async_trait]
impl Llm for SwitchableModel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        self.require()?.generate_content(req, stream).await
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        &self.adapter
    }

    fn uses_interactions_api(&self) -> bool {
        self.current().is_some_and(|model| model.uses_interactions_api())
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        self.current().and_then(|model| model.max_cost_micro_usd(request))
    }

    fn native_web_search(&self) -> Option<NativeWebSearch> {
        self.current().and_then(|model| model.native_web_search())
    }
}

/// The schema adapter of whichever model backs a [`SwitchableModel`].
#[derive(Clone)]
struct CurrentAdapter(Arc<SwitchState>);

impl std::fmt::Debug for CurrentAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CurrentAdapter")
    }
}

impl SchemaAdapter for CurrentAdapter {
    fn normalize_schema(&self, schema: Value) -> Value {
        match self.0.current() {
            Some(model) => model.schema_adapter().normalize_schema(schema),
            None => adk_core::GenericSchemaAdapter.normalize_schema(schema),
        }
    }

    fn normalize_tool_name<'a>(&self, name: &'a str) -> Cow<'a, str> {
        match self.0.current() {
            Some(model) => model.schema_adapter().normalize_tool_name(name),
            None => adk_core::GenericSchemaAdapter.normalize_tool_name(name),
        }
    }

    fn empty_schema(&self) -> Value {
        match self.0.current() {
            Some(model) => model.schema_adapter().empty_schema(),
            None => adk_core::GenericSchemaAdapter.empty_schema(),
        }
    }

    fn parameters_field(&self) -> &'static str {
        match self.0.current() {
            Some(model) => model.schema_adapter().parameters_field(),
            None => adk_core::GenericSchemaAdapter.parameters_field(),
        }
    }
}

/// Builds the model that backs a provider from a client's settings.
pub type ModelFactory =
    dyn Fn(&ProviderSettings) -> Result<Arc<dyn Llm>, AcpServerError> + Send + Sync;

/// One provider of [`ModelProviders`]: a [`SwitchableModel`] and the factory
/// that builds its model from a client's settings.
pub struct ModelProvider {
    id: ProviderId,
    model: SwitchableModel,
    factory: Arc<ModelFactory>,
    supported: Vec<LlmProtocol>,
    required: bool,
    current: Option<ProviderCurrentConfig>,
}

impl ModelProvider {
    /// A provider that switches `model` to what `factory` builds.
    ///
    /// It supports no protocols until [`supports`](Self::supports) adds them,
    /// is not required, and reports no current target until
    /// [`current`](Self::current) sets one.
    pub fn new(
        id: impl Into<ProviderId>,
        model: SwitchableModel,
        factory: impl Fn(&ProviderSettings) -> Result<Arc<dyn Llm>, AcpServerError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            model,
            factory: Arc::new(factory),
            supported: Vec::new(),
            required: false,
            current: None,
        }
    }

    /// Accept settings that use `protocol`.
    pub fn supports(mut self, protocol: LlmProtocol) -> Self {
        self.supported.push(protocol);
        self
    }

    /// Refuse to disable this provider.
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// The routing target the initial model uses, reported by `providers/list`.
    pub fn current(mut self, api_type: LlmProtocol, base_url: impl Into<String>) -> Self {
        self.current = Some(ProviderCurrentConfig::new(api_type, base_url));
        self
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            self.id.clone(),
            self.supported.clone(),
            self.required,
            self.current.clone(),
        )
    }
}

/// An [`AcpProviders`] hook over [`SwitchableModel`]s.
///
/// `providers/set` builds a model with the provider's factory and switches
/// the provider's model to it; `providers/disable` switches the model off so
/// its requests fail until a later `providers/set`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::server::{LlmProtocol, ModelProvider, ModelProviders, SwitchableModel};
///
/// let model = SwitchableModel::new(default_model);
/// let agent = LlmAgentBuilder::new("coder").model(Arc::new(model.clone())).build()?;
/// let providers = ModelProviders::new().provider(
///     ModelProvider::new("main", model, |settings| build_model(settings))
///         .supports(LlmProtocol::OpenAi)
///         .required(true)
///         .current(LlmProtocol::OpenAi, "https://api.openai.com/v1"),
/// );
/// let config = AcpServerConfigBuilder::new()
///     .agent(Arc::new(agent))
///     .session_service(sessions)
///     .providers(Arc::new(providers))
///     .build()?;
/// ```
#[derive(Default)]
pub struct ModelProviders {
    providers: Mutex<Vec<ModelProvider>>,
}

impl ModelProviders {
    /// No providers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `provider`; a later provider with the same id replaces it.
    pub fn provider(self, provider: ModelProvider) -> Self {
        {
            let mut providers = self.lock();
            providers.retain(|existing| existing.id != provider.id);
            providers.push(provider);
        }
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<ModelProvider>> {
        self.providers.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl AcpProviders for ModelProviders {
    async fn list(&self) -> Result<Vec<ProviderInfo>, AcpServerError> {
        Ok(self.lock().iter().map(ModelProvider::info).collect())
    }

    async fn set(&self, settings: ProviderSettings) -> Result<(), AcpServerError> {
        let (factory, model) = {
            let providers = self.lock();
            let provider = providers
                .iter()
                .find(|provider| provider.id == settings.provider_id)
                .ok_or_else(|| {
                    AcpServerError::MalformedMessage(format!(
                        "unknown provider: {}",
                        settings.provider_id
                    ))
                })?;
            (provider.factory.clone(), provider.model.clone())
        };
        let built = factory(&settings)?;
        model.replace(built);
        if let Some(provider) =
            self.lock().iter_mut().find(|provider| provider.id == settings.provider_id)
        {
            provider.current = Some(settings.current());
        }
        Ok(())
    }

    async fn disable(&self, provider_id: &ProviderId) -> Result<(), AcpServerError> {
        if let Some(provider) = self.lock().iter_mut().find(|provider| provider.id == *provider_id)
        {
            provider.model.disable();
            provider.current = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use adk_core::{Content, LlmResponse};
    use futures::StreamExt;

    use super::*;

    struct Named(&'static str);

    #[async_trait]
    impl Llm for Named {
        fn name(&self) -> &str {
            self.0
        }

        async fn generate_content(&self, _: LlmRequest, _: bool) -> AdkResult<LlmResponseStream> {
            let response = LlmResponse::new(Content::new("model").with_text(self.0));
            Ok(Box::pin(futures::stream::once(async move { Ok(response) })))
        }
    }

    async fn answer(model: &SwitchableModel) -> AdkResult<String> {
        let mut stream = model.generate_content(LlmRequest::new("m", Vec::new()), false).await?;
        let response = stream.next().await.expect("a response")?;
        Ok(response.content.expect("content").parts[0].text().unwrap_or_default().to_string())
    }

    fn providers(main: &SwitchableModel, helper: &SwitchableModel) -> ModelProviders {
        ModelProviders::new()
            .provider(
                ModelProvider::new("main", main.clone(), |_| Ok(Arc::new(Named("gateway"))))
                    .supports(LlmProtocol::OpenAi)
                    .required(true)
                    .current(LlmProtocol::OpenAi, "https://api.example.com/v1"),
            )
            .provider(
                ModelProvider::new("helper", helper.clone(), |_| Ok(Arc::new(Named("helper"))))
                    .supports(LlmProtocol::Anthropic),
            )
    }

    fn model(name: &'static str) -> SwitchableModel {
        SwitchableModel::new(Arc::new(Named(name)))
    }

    #[tokio::test]
    async fn set_switches_the_model_and_reports_the_new_target_without_headers() {
        let model = model("default");
        let providers = providers(&model, &self::model("helper-default"));
        assert_eq!(answer(&model).await.unwrap(), "default");

        let request = SetProviderRequest::new("main", LlmProtocol::OpenAi, "https://gw.example/v1")
            .headers(HashMap::from([("authorization".to_string(), "Bearer secret".to_string())]));
        set_provider(&providers, request).await.unwrap();
        assert_eq!(answer(&model).await.unwrap(), "gateway");
        assert_eq!(model.name(), "default", "the name stays the original model's");

        let listed = providers.list().await.unwrap();
        let main = listed.iter().find(|provider| &*provider.provider_id.0 == "main").unwrap();
        assert_eq!(main.current.as_ref().unwrap().base_url, "https://gw.example/v1");
        assert!(!serde_json::to_string(&listed).unwrap().contains("secret"));
    }

    #[tokio::test]
    async fn requests_are_checked_against_the_listing() {
        let (main, helper) = (model("default"), model("helper-default"));
        let providers = providers(&main, &helper);
        let unknown = SetProviderRequest::new("nope", LlmProtocol::OpenAi, "https://x");
        assert!(set_provider(&providers, unknown).await.is_err());
        let wrong_protocol = SetProviderRequest::new("main", LlmProtocol::Bedrock, "https://x");
        assert!(set_provider(&providers, wrong_protocol).await.is_err());
        let no_url = SetProviderRequest::new("main", LlmProtocol::OpenAi, " ");
        assert!(set_provider(&providers, no_url).await.is_err());

        assert!(disable_provider(&providers, DisableProviderRequest::new("main")).await.is_err());
        disable_provider(&providers, DisableProviderRequest::new("nope")).await.unwrap();
        disable_provider(&providers, DisableProviderRequest::new("helper")).await.unwrap();
        assert!(answer(&helper).await.is_err(), "a disabled provider's model fails requests");
        assert_eq!(answer(&main).await.unwrap(), "default", "other providers keep working");
        let listed = providers.list().await.unwrap();
        let helper = listed.iter().find(|provider| &*provider.provider_id.0 == "helper").unwrap();
        assert!(helper.current.is_none());
    }

    #[test]
    fn settings_debug_redacts_header_values() {
        let settings = ProviderSettings::new("main", LlmProtocol::OpenAi, "https://x")
            .headers(HashMap::from([("authorization".to_string(), "Bearer secret".to_string())]));
        let debug = format!("{settings:?}");
        assert!(debug.contains("authorization") && !debug.contains("secret"));
    }
}
