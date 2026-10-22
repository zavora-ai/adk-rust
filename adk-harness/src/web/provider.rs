//! Provider-executed search and URL tools (spec 3.1 §7.20 "Provider-native search and
//! fetch", §9.2 I49).
//!
//! These run inside the model, outside the governed path. The toolset declares them only
//! to a run that starts in Ask or Auto, for the provider the harness model's client serves
//! only, carries the allowlist where the provider supports one, says so in the request,
//! records what the provider searched after each response, and ends the turn when the mode
//! tightens while one is live.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use adk_core::audit_log::AuditActor;
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, EventStream, InvocationContext, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, NativeWebSearch, NativeWebSearchTool, Part,
    ReadonlyContext, Result, SchemaAdapter, Tool, Toolset, async_trait,
};
use serde_json::{Value, json};

use super::{ProviderWebTools, WebSearchPath, hex_sha256, redact_secrets, url_host};
use crate::audit::HarnessAudit;
use crate::mode::{HarnessMode, ModeStore};

/// Error code of a turn ended because the mode tightened while a provider tool was live.
pub const PROVIDER_WEB_TIGHTENED_CODE: &str = "harness.web.provider_tightened";

/// URLs one `harness.web.searched` record lists at most.
const RECORDED_URLS_MAX: usize = 50;

/// The text that opens the request banner, so a retry without the tools drops it.
const BANNER_LEAD: &str = "Provider-native web tools are live for this turn";

struct Entry {
    provider: &'static str,
    tool: Arc<dyn Tool>,
    /// The allowlist forwarded to the provider, when it takes one and one is configured.
    allowlist: Option<Vec<String>>,
    /// Whether the provider takes an allowlist on this tool.
    allowlist_supported: bool,
    /// Whether the tool searches (`url_context` reads the URLs the prompt names).
    search: bool,
}

/// The provider-executed web tools the operator enabled, declared per run (spec 3.1 §7.20).
pub(crate) struct ProviderWebToolset {
    entries: Vec<Entry>,
    /// Whether the operator configured an allowlist, so the banner says when a provider
    /// cannot take it.
    allowlist_configured: bool,
    /// Why the model provider's search was requested but not attached.
    unavailable: Option<String>,
    mode_store: Arc<dyn ModeStore>,
    default_mode: HarnessMode,
    /// Invocations the tools were declared to.
    attached: Mutex<HashSet<String>>,
    /// Invocations whose `harness.web.provider_enabled` records were written.
    recorded: Mutex<HashSet<String>>,
    /// Why the provider refused its built-ins, once a request without them succeeded; the
    /// tools stay off from then on.
    refused: OnceLock<String>,
}

/// The built-in that declares `search` to the provider, with `domains` forwarded where the
/// provider takes an allowlist.
fn search_entry(search: NativeWebSearch, domains: Option<&Vec<String>>) -> Option<Entry> {
    let allowlist = domains.filter(|_| search.allowed_domains).cloned();
    let tool: Arc<dyn Tool> = match search.tool {
        NativeWebSearchTool::GoogleSearch => Arc::new(adk_tool::builtin::GoogleSearchTool::new()),
        NativeWebSearchTool::AnthropicWebSearch => {
            let mut tool = adk_tool::builtin::WebSearchTool::new();
            if let Some(domains) = &allowlist {
                tool = tool.with_allowed_domains(domains.clone());
            }
            Arc::new(tool)
        }
        NativeWebSearchTool::OpenAiWebSearch => {
            let mut tool = adk_tool::builtin::OpenAIWebSearchTool::new();
            if let Some(domains) = &allowlist {
                tool = tool.with_allowed_domains(domains.clone());
            }
            Arc::new(tool)
        }
        NativeWebSearchTool::DeepSeekWebSearch => {
            Arc::new(adk_tool::builtin::DeepSeekWebSearchTool::new())
        }
        // A search kind this harness does not know is not attached.
        _ => return None,
    };
    Some(Entry {
        provider: search.provider,
        tool,
        allowlist,
        allowlist_supported: search.allowed_domains,
        search: true,
    })
}

impl ProviderWebToolset {
    /// Builds the toolset for `model`, keeping the tools of the provider its client serves
    /// only (spec 3.1 §7.20): the provider is read from [`Llm::native_web_search`], never
    /// from the model's output. A configured search provider replaces the model provider's
    /// search; a tool enabled for another provider is skipped with a warning.
    pub(crate) fn new(
        tools: ProviderWebTools,
        model: &dyn Llm,
        allowlist: Vec<String>,
        search_provider_configured: bool,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Result<Self> {
        let native = model.native_web_search();
        let allowlist_configured = !allowlist.is_empty();
        let domains = allowlist_configured.then_some(allowlist);
        let mut entries: Vec<Entry> = Vec::new();
        let mut skipped = Vec::new();
        let mut unavailable = None;
        let mut attach =
            |entry: Option<Entry>, label: &'static str, skipped: &mut Vec<_>| match entry {
                Some(entry) if !entries.iter().any(|e| e.tool.name() == entry.tool.name()) => {
                    entries.push(entry);
                }
                Some(_) => {}
                None => skipped.push(label),
            };
        if tools.native_search && !search_provider_configured {
            match native {
                Some(search) => {
                    attach(search_entry(search, domains.as_ref()), "native search", &mut skipped)
                }
                None => {
                    unavailable = Some(format!(
                        "the client of {} serves no provider-native search; configure a search \
                         provider for web_search",
                        model.name()
                    ));
                }
            }
        }
        let matching = |tool: NativeWebSearchTool| native.filter(|search| search.tool == tool);
        if tools.anthropic_web_search {
            let entry = matching(NativeWebSearchTool::AnthropicWebSearch)
                .and_then(|search| search_entry(search, domains.as_ref()));
            attach(entry, "anthropic web_search", &mut skipped);
        }
        if tools.openai_web_search {
            let entry = matching(NativeWebSearchTool::OpenAiWebSearch)
                .and_then(|search| search_entry(search, domains.as_ref()));
            attach(entry, "openai web_search", &mut skipped);
        }
        if tools.gemini_google_search {
            let entry = matching(NativeWebSearchTool::GoogleSearch)
                .and_then(|search| search_entry(search, domains.as_ref()));
            attach(entry, "gemini google_search", &mut skipped);
        }
        if tools.gemini_url_context {
            let entry = matching(NativeWebSearchTool::GoogleSearch).map(|search| Entry {
                provider: search.provider,
                tool: Arc::new(adk_tool::builtin::UrlContextTool::new()),
                allowlist: None,
                allowlist_supported: false,
                search: false,
            });
            attach(entry, "gemini url_context", &mut skipped);
        }
        if !skipped.is_empty() {
            tracing::warn!(
                model = model.name(),
                native = ?native.map(|search| search.tool),
                skipped = ?skipped,
                "provider web tools the model's client does not serve are not attached"
            );
        }
        Ok(Self {
            entries,
            allowlist_configured,
            unavailable,
            mode_store,
            default_mode,
            attached: Mutex::new(HashSet::new()),
            recorded: Mutex::new(HashSet::new()),
            refused: OnceLock::new(),
        })
    }

    /// Names of the attached tools, in order.
    pub(crate) fn tool_names(&self) -> Vec<String> {
        self.entries.iter().map(|entry| entry.tool.name().to_string()).collect()
    }

    /// Which path `web_search` takes when no search provider is configured: the attached
    /// provider search, or why there is none.
    pub(crate) fn search_path(&self) -> Option<WebSearchPath> {
        if let Some(entry) = self.entries.iter().find(|entry| entry.search) {
            return Some(WebSearchPath::Provider {
                provider: entry.provider.to_string(),
                tool: entry.tool.name().to_string(),
                allowlist: entry.allowlist.clone(),
                allowlist_supported: entry.allowlist_supported,
            });
        }
        self.unavailable.clone().map(|reason| WebSearchPath::None { reason })
    }

    async fn mode(&self, app: &str, user: &str, session: &str, invocation: &str) -> HarnessMode {
        match self.mode_store.get(app, user, session).await {
            Ok(Some(record)) => record.mode_for_invocation(invocation),
            Ok(None) => self.default_mode,
            Err(_) => HarnessMode::ReadOnly,
        }
    }

    /// The request banner naming which path is governed (spec 3.1 §7.20).
    pub(crate) fn banner(&self) -> String {
        let live: Vec<String> = self
            .entries
            .iter()
            .map(|entry| match &entry.allowlist {
                Some(domains) => {
                    format!(
                        "{} {} (allowlist: {})",
                        entry.provider,
                        entry.tool.name(),
                        domains.join(", ")
                    )
                }
                None if self.allowlist_configured && !entry.allowlist_supported => format!(
                    "{} {} (every domain: {} takes no allowlist)",
                    entry.provider,
                    entry.tool.name(),
                    entry.provider
                ),
                None => format!("{} {} (every domain)", entry.provider, entry.tool.name()),
            })
            .collect();
        format!(
            "{BANNER_LEAD}: {}. They run inside the model outside the harness's governance; \
             web_fetch is the governed path and its results are screened.",
            live.join("; ")
        )
    }

    fn audit(ctx: &dyn InvocationContext) -> Option<HarnessAudit> {
        let trail = ctx.run_config().audit.clone()?;
        Some(HarnessAudit::new(
            trail,
            AuditActor::new(&ctx.acting_context(), ctx.invocation_id().to_string()),
            ctx.session_id(),
        ))
    }

    /// Records `harness.web.provider_enabled` once per run that starts in Ask or Auto
    /// (spec 3.1 §9.3).
    ///
    /// # Errors
    ///
    /// Returns the audit error under `AuditFailureMode::Block`.
    pub(crate) async fn record_run_start(&self, ctx: &dyn InvocationContext) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let mode =
            self.mode(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id()).await;
        if !matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
            return Ok(());
        }
        let first = self
            .recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ctx.invocation_id().to_string());
        let Some(audit) = Self::audit(ctx) else {
            return Ok(());
        };
        if !first {
            return Ok(());
        }
        for entry in &self.entries {
            audit
                .record(
                    "harness.web.provider_enabled",
                    json!({
                        "provider": entry.provider,
                        "tool": entry.tool.name(),
                        "allowlist": entry.allowlist,
                        "mode": mode,
                    }),
                )
                .await?;
        }
        Ok(())
    }

    /// Records, after each model response, what the provider searched and cited as
    /// `harness.web.searched` with `source: provider` (spec 3.1 §7.20, §9.3): the search
    /// ran inside the model and was never held, so the audit trail shows it afterwards.
    pub(crate) fn record_searches(
        self: &Arc<Self>,
        ctx: Arc<dyn InvocationContext>,
        events: EventStream,
    ) -> EventStream {
        use futures::StreamExt as _;
        let Some(entry) = self.entries.iter().find(|entry| entry.search) else {
            return events;
        };
        let (provider, tool) = (entry.provider, entry.tool.name().to_string());
        // One record per model response: streamed chunks share the response's event id and
        // are merged until its complete event.
        let pending: Arc<Mutex<HashMap<String, ProviderSearches>>> = Arc::default();
        Box::pin(events.then(move |event| {
            let (ctx, pending, tool) = (ctx.clone(), pending.clone(), tool.clone());
            async move {
                let event = event?;
                let found = provider_searches(&event.llm_response);
                let complete = {
                    let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
                    if !found.is_empty() {
                        pending.entry(event.id.clone()).or_default().merge(found);
                    }
                    if event.llm_response.partial { None } else { pending.remove(&event.id) }
                };
                if let Some(searches) = complete
                    && let Some(audit) = Self::audit(ctx.as_ref())
                {
                    audit.record("harness.web.searched", searches.record(provider, &tool)).await?;
                }
                Ok(event)
            }
        }))
    }
}

/// The harness model, retried once without the provider-executed web tools when the
/// provider refuses them (spec 3.1 §7.20).
///
/// A provider may refuse a request only because it declares a built-in: no search quota on
/// the account, no permission, or a model that does not take the tool. Such a refusal —
/// a rate-limit, forbidden, invalid-input or unsupported error before the first response —
/// gets the same request again without the tools and the banner; when that succeeds, the
/// tools stay off for this harness and a warning names the reason, so a provider default
/// never makes the harness unusable. Every other method forwards to the wrapped model.
pub(crate) struct ProviderSearchFallback {
    inner: Arc<dyn Llm>,
    toolset: Arc<ProviderWebToolset>,
}

impl ProviderSearchFallback {
    pub(crate) fn new(inner: Arc<dyn Llm>, toolset: Arc<ProviderWebToolset>) -> Self {
        Self { inner, toolset }
    }

    /// Calls the model and reads the first response, so a refusal sent as the stream's
    /// first item is seen before anything reaches the agent.
    async fn first(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> std::result::Result<LlmResponseStream, AdkError> {
        use futures::StreamExt as _;
        let mut responses = self.inner.generate_content(request, stream).await?;
        match responses.next().await {
            Some(Err(error)) => Err(error),
            Some(Ok(head)) => {
                Ok(Box::pin(futures::stream::once(async move { Ok(head) }).chain(responses)))
            }
            None => Ok(Box::pin(futures::stream::empty())),
        }
    }
}

#[async_trait]
impl Llm for ProviderSearchFallback {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        mut request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        let names = self.toolset.tool_names();
        if !names.iter().any(|name| request.tools.contains_key(name)) {
            return self.inner.generate_content(request, stream).await;
        }
        let error = match self.first(request.clone(), stream).await {
            Ok(responses) => return Ok(responses),
            Err(error) => error,
        };
        if !matches!(
            error.category,
            ErrorCategory::RateLimited
                | ErrorCategory::Forbidden
                | ErrorCategory::InvalidInput
                | ErrorCategory::Unsupported
        ) {
            return Err(error);
        }
        for name in &names {
            request.tools.remove(name);
        }
        request.contents.retain(|content| {
            !content
                .parts
                .iter()
                .any(|part| matches!(part, Part::Text { text, .. } if text.contains(BANNER_LEAD)))
        });
        let responses = self.first(request, stream).await?;
        let reason = format!("{} refused its built-in web tools: {error}", self.inner.name());
        tracing::warn!(
            model = self.inner.name(),
            tools = ?names,
            error = %error,
            "the provider refused its built-in web tools; they stay off for this harness and \
             web_search needs a configured search provider"
        );
        let _ = self.toolset.refused.set(reason);
        Ok(responses)
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }

    fn uses_interactions_api(&self) -> bool {
        self.inner.uses_interactions_api()
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        self.inner.max_cost_micro_usd(request)
    }

    fn native_web_search(&self) -> Option<NativeWebSearch> {
        self.inner.native_web_search()
    }
}

/// What a provider reported searching and citing in one model response.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderSearches {
    pub(crate) queries: Vec<String>,
    pub(crate) urls: Vec<String>,
}

impl ProviderSearches {
    pub(crate) fn is_empty(&self) -> bool {
        self.queries.is_empty() && self.urls.is_empty()
    }

    fn merge(&mut self, other: Self) {
        for query in other.queries {
            push_distinct(&mut self.queries, query);
        }
        for url in other.urls {
            push_distinct(&mut self.urls, url);
        }
    }

    /// The `harness.web.searched` fields: queries and URLs with secret patterns redacted
    /// (I30), the queries' digests, and the cited hosts.
    fn record(&self, provider: &str, tool: &str) -> Value {
        let mut hosts: Vec<String> = Vec::new();
        for url in &self.urls {
            if let Some(host) = url_host(url) {
                push_distinct(&mut hosts, host);
            }
        }
        json!({
            "source": "provider",
            "provider": provider,
            "tool": tool,
            "queries": self.queries.iter().map(|q| redact_secrets(q).0).collect::<Vec<_>>(),
            "queryDigests": self.queries.iter().map(|q| hex_sha256(q.as_bytes())).collect::<Vec<_>>(),
            "results": self.urls.len(),
            "urls": self
                .urls
                .iter()
                .take(RECORDED_URLS_MAX)
                .map(|url| redact_secrets(url).0)
                .collect::<Vec<_>>(),
            "hosts": hosts,
        })
    }
}

fn push_distinct(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

/// The searches and cited URLs a provider reported in `response` (spec 3.1 §7.20):
/// Anthropic `server_tool_use` and `web_search_tool_result` blocks, OpenAI and DeepSeek
/// `web_search_call` items, and Gemini `groundingMetadata.webSearchQueries` and grounding
/// chunks.
pub(crate) fn provider_searches(response: &LlmResponse) -> ProviderSearches {
    let mut found = ProviderSearches::default();
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
    for part in response.content.iter().flat_map(|content| &content.parts) {
        match part {
            Part::ServerToolCall { server_tool_call: call } => {
                // Anthropic `server_tool_use`.
                if call.get("name").and_then(Value::as_str) == Some("web_search")
                    && let Some(query) = text(call.pointer("/input/query"))
                {
                    push_distinct(&mut found.queries, query);
                }
                // OpenAI and DeepSeek Responses `web_search_call`.
                if call.get("type").and_then(Value::as_str) == Some("web_search_call")
                    && let Some(action) = call.get("action")
                {
                    if let Some(query) = text(action.get("query")) {
                        push_distinct(&mut found.queries, query);
                    }
                    for query in
                        action.get("queries").and_then(Value::as_array).into_iter().flatten()
                    {
                        if let Some(query) = text(Some(query)) {
                            push_distinct(&mut found.queries, query);
                        }
                    }
                    for source in
                        action.get("sources").and_then(Value::as_array).into_iter().flatten()
                    {
                        if let Some(url) = text(source.get("url")) {
                            push_distinct(&mut found.urls, url);
                        }
                    }
                }
            }
            // Anthropic `web_search_tool_result`.
            Part::ServerToolResponse { server_tool_response: result } => {
                for item in result.get("content").and_then(Value::as_array).into_iter().flatten() {
                    if item.get("type").and_then(Value::as_str) == Some("web_search_result")
                        && let Some(url) = text(item.get("url"))
                    {
                        push_distinct(&mut found.urls, url);
                    }
                }
            }
            _ => {}
        }
    }
    // Gemini grounding metadata, which the client keeps in `provider_metadata`.
    if let Some(metadata) = &response.provider_metadata {
        for query in
            metadata.get("webSearchQueries").and_then(Value::as_array).into_iter().flatten()
        {
            if let Some(query) = text(Some(query)) {
                push_distinct(&mut found.queries, query);
            }
        }
        for chunk in metadata.get("groundingChunks").and_then(Value::as_array).into_iter().flatten()
        {
            if let Some(uri) = text(chunk.pointer("/web/uri")) {
                push_distinct(&mut found.urls, uri);
            }
        }
    }
    found
}

#[async_trait]
impl Toolset for ProviderWebToolset {
    fn name(&self) -> &str {
        "harness-provider-web"
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let mode =
            self.mode(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id()).await;
        let mut attached = self.attached.lock().unwrap_or_else(|e| e.into_inner());
        if attached.len() > 4096 {
            attached.clear();
        }
        // Declared only to a run that starts in Ask or Auto (I49): the first request of
        // the invocation decides, and a later tightening ends the turn below.
        let live = if attached.contains(ctx.invocation_id()) {
            true
        } else if matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
            attached.insert(ctx.invocation_id().to_string());
            true
        } else {
            false
        };
        Ok(if live && self.refused.get().is_none() {
            self.entries.iter().map(|entry| entry.tool.clone()).collect()
        } else {
            Vec::new()
        })
    }

    async fn process_llm_request(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        let live =
            self.attached.lock().unwrap_or_else(|e| e.into_inner()).contains(ctx.invocation_id());
        if !live || self.refused.get().is_some() {
            return Ok(());
        }
        let mode =
            self.mode(ctx.app_name(), ctx.user_id(), ctx.session_id(), ctx.invocation_id()).await;
        if matches!(mode, HarnessMode::ReadOnly | HarnessMode::Plan) {
            // Ending the turn is the only way to retire a tool the provider already holds.
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Forbidden,
                PROVIDER_WEB_TIGHTENED_CODE,
                format!(
                    "the mode tightened to {mode} while a provider-executed web tool was live; \
                     the turn ends so no ungoverned fetch runs (spec 3.1 §7.20)"
                ),
            ));
        }
        request.contents.push(
            Content::new("user")
                .with_text(format!("<system-reminder>\n{}\n</system-reminder>", self.banner())),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(parts: Vec<Part>, metadata: Option<Value>) -> LlmResponse {
        let mut response = LlmResponse::new(Content { role: "model".into(), parts });
        response.provider_metadata = metadata;
        response
    }

    #[test]
    fn anthropic_server_tool_blocks_yield_the_query_and_the_result_urls() {
        let found = provider_searches(&response(
            vec![
                Part::ServerToolCall {
                    server_tool_call: json!({
                        "id": "srvtoolu_1", "name": "web_search",
                        "input": { "query": "rust 1.95 release notes" }
                    }),
                },
                Part::ServerToolResponse {
                    server_tool_response: json!({
                        "tool_use_id": "srvtoolu_1",
                        "content": [
                            { "type": "web_search_result", "url": "https://blog.rust-lang.org/a", "title": "A", "encrypted_content": "x" },
                            { "type": "web_search_result", "url": "https://doc.rust-lang.org/b", "title": "B", "encrypted_content": "y" }
                        ]
                    }),
                },
                Part::Text { text: "Rust 1.95 shipped.".into() },
            ],
            None,
        ));
        assert_eq!(
            found,
            ProviderSearches {
                queries: vec!["rust 1.95 release notes".into()],
                urls: vec![
                    "https://blog.rust-lang.org/a".into(),
                    "https://doc.rust-lang.org/b".into()
                ],
            }
        );
    }

    #[test]
    fn a_responses_web_search_call_yields_its_queries_and_sources() {
        let found = provider_searches(&response(
            vec![Part::ServerToolCall {
                server_tool_call: json!({
                    "type": "web_search_call", "id": "ws_1", "status": "completed",
                    "action": {
                        "type": "search", "query": "adk-rust harness",
                        "queries": ["adk-rust harness", "adk-rust docs"],
                        "sources": [{ "type": "url", "url": "https://docs.rs/adk-rust" }]
                    }
                }),
            }],
            None,
        ));
        assert_eq!(
            found,
            ProviderSearches {
                queries: vec!["adk-rust harness".into(), "adk-rust docs".into()],
                urls: vec!["https://docs.rs/adk-rust".into()],
            }
        );
    }

    #[test]
    fn gemini_grounding_metadata_yields_queries_and_chunk_uris() {
        let found = provider_searches(&response(
            vec![Part::Text { text: "answer".into() }],
            Some(json!({
                "webSearchQueries": ["latest rust version"],
                "groundingChunks": [
                    { "web": { "uri": "https://vertexaisearch.cloud.google.com/grounding-api-redirect/abc", "title": "rust-lang.org" } },
                    { "maps": { "uri": "https://maps.google.com/x" } }
                ],
                "content_complete": true
            })),
        ));
        assert_eq!(
            found,
            ProviderSearches {
                queries: vec!["latest rust version".into()],
                urls: vec![
                    "https://vertexaisearch.cloud.google.com/grounding-api-redirect/abc".into()
                ],
            }
        );
    }

    #[test]
    fn other_server_tools_and_plain_text_yield_nothing() {
        let found = provider_searches(&response(
            vec![
                Part::ServerToolCall {
                    server_tool_call: json!({ "id": "srvtoolu_2", "name": "code_execution", "input": { "code": "1" } }),
                },
                Part::ServerToolResponse {
                    server_tool_response: json!({ "type": "web_fetch_tool_result", "content": { "url": "https://x.test" } }),
                },
            ],
            Some(json!({ "logprobs": [] })),
        ));
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn the_record_redacts_secret_patterns_and_keeps_digests_and_hosts() {
        let searches = ProviderSearches {
            queries: vec!["key AKIAIOSFODNN7EXAMPLE".into()],
            urls: vec!["https://docs.rs/a".into(), "https://docs.rs/b".into()],
        };
        let record = searches.record("anthropic", "web_search");
        assert_eq!(record["source"], "provider");
        assert!(!record["queries"][0].as_str().unwrap().contains("AKIA"), "{record}");
        assert_eq!(record["queryDigests"][0], hex_sha256(b"key AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(record["results"], 2);
        assert_eq!(record["hosts"], json!(["docs.rs"]));
    }
}
