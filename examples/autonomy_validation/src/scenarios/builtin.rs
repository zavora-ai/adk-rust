//! Provider-executed built-in tools under the run's policy (Phase 1 deferred): a policy that
//! denies web search keeps the tool out of the request, so the provider never searches; a
//! policy that allows it attaches the tool, and every reported search is counted and priced.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, BUILTIN_TOOLS_WITHHELD_KEY, BuiltinToolWithholding, DeclarativePolicy, Llm, LlmRequest,
    LlmResponseStream, Result as AdkResult, RunBudget, RunConfig, SchemaAdapter, Tool,
    UsageMetadata, WithheldBuiltinTool,
};
use adk_model::pricing::PricingCatalog;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;

use crate::common::{Config, Provider, Verdict, brief, fail, run_turn};

const USER: &str = "user-builtin";
const PROMPT: &str = "Search the web for the most recent stable Rust release and its release date. \
                      Answer in one sentence.";

/// Records the tool names of every request it forwards.
struct RequestRecorder {
    inner: Arc<dyn Llm>,
    tools: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl Llm for RequestRecorder {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        let mut names: Vec<String> = req.tools.keys().cloned().collect();
        names.sort();
        self.tools.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(names);
        self.inner.generate_content(req, stream).await
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }
}

struct Setup {
    model: Arc<dyn Llm>,
    model_id: String,
    tool: Arc<dyn Tool>,
    vendor: &'static str,
}

fn setup(cfg: &Config, provider: Provider) -> anyhow::Result<Option<Setup>> {
    Ok(Some(match provider {
        Provider::OpenAi => Setup {
            model: cfg.model(provider)?,
            model_id: cfg.openai_model.clone(),
            tool: Arc::new(adk_tool::OpenAIWebSearchTool::new()),
            vendor: "openai",
        },
        Provider::Anthropic => Setup {
            model: cfg.anthropic(&cfg.anthropic_web_model, |config| config)?,
            model_id: cfg.anthropic_web_model.clone(),
            tool: Arc::new(adk_tool::WebSearchTool::new().with_max_uses(2)),
            vendor: "anthropic",
        },
        Provider::Gemini => Setup {
            model: cfg.gemini(&cfg.gemini_search_model)?,
            model_id: cfg.gemini_search_model.clone(),
            tool: Arc::new(adk_tool::GoogleSearchTool::new()),
            vendor: "gemini",
        },
        // Hosted tools route Chat Completions requests to the Responses API, covered by `openai`.
        Provider::OpenAiChat => return Ok(None),
    }))
}

/// What one run sent and what the provider reported back.
struct Observed {
    requests: Vec<Vec<String>>,
    uses: u64,
    usages: Vec<UsageMetadata>,
    withheld: Vec<WithheldBuiltinTool>,
    error: Option<String>,
}

async fn observe(
    setup: &Setup,
    session_id: &str,
    policy: DeclarativePolicy,
) -> anyhow::Result<Observed> {
    let recorder = Arc::new(RequestRecorder {
        inner: Arc::clone(&setup.model),
        tools: Mutex::new(Vec::new()),
    });
    let agent = LlmAgentBuilder::new("researcher")
        .model(Arc::clone(&recorder) as Arc<dyn Llm>)
        .instruction("Use web search for questions about current facts. Answer in one sentence.")
        .tool(Arc::clone(&setup.tool))
        .build()?;
    let app = "autonomy-builtin";
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
        .tool_policy(Arc::new(policy))
        .run_config(RunConfig::builder().budget(RunBudget::new().max_tool_calls(20)).build())
        .build()?;

    let turn = run_turn(&runner, USER, session_id, PROMPT).await;
    let finals = turn.events.iter().filter(|event| !event.llm_response.partial);
    let usages: Vec<UsageMetadata> =
        finals.clone().filter_map(|event| event.llm_response.usage_metadata.clone()).collect();
    let withheld = finals
        .filter_map(|event| event.provider_metadata.get(BUILTIN_TOOLS_WITHHELD_KEY))
        .flat_map(|json| serde_json::from_str::<Vec<WithheldBuiltinTool>>(json).unwrap_or_default())
        .collect();
    Ok(Observed {
        requests: recorder.tools.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone(),
        uses: usages.iter().map(UsageMetadata::builtin_tool_use_count).sum(),
        usages,
        withheld,
        error: turn.error,
    })
}

pub async fn builtin_tool_policy(cfg: &Config, provider: Provider) -> Verdict {
    match run(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn run(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let Some(setup) = setup(cfg, provider)? else {
        return Ok(Verdict::Skip("hosted tools use the Responses API; see `openai`".to_string()));
    };
    let name = setup.tool.name().to_string();

    // A default-deny policy that permits nothing: the tool never reaches the provider.
    let denied = observe(&setup, "builtin-denied", DeclarativePolicy::builder().build()).await?;
    if let Some(error) = &denied.error {
        return Ok(Verdict::Fail(format!("denied run failed: {}", brief(error))));
    }
    if denied.requests.is_empty() || denied.requests.iter().any(|tools| tools.contains(&name)) {
        return Ok(Verdict::Fail(format!(
            "denied run requests carried tools {:?}",
            denied.requests
        )));
    }
    if denied.uses != 0 {
        return Ok(Verdict::Fail(format!(
            "provider reported {} built-in uses under deny",
            denied.uses
        )));
    }
    if !denied
        .withheld
        .iter()
        .any(|w| w.tool_name == name && w.cause == BuiltinToolWithholding::PolicyDenied)
    {
        return Ok(Verdict::Fail("no withheld record on the denied run's events".to_string()));
    }

    // Read-only tools allowed: the tool is attached and its uses are metered and priced.
    let allowed =
        observe(&setup, "builtin-allowed", DeclarativePolicy::builder().allow_read_only().build())
            .await?;
    if let Some(error) = &allowed.error {
        // The denied run made the same model call without the tool, so a quota error here
        // is the account's search grounding quota, not the model's.
        if provider == Provider::Gemini && error.contains("RESOURCE_EXHAUSTED") {
            return Ok(Verdict::Skip(format!(
                "denied half passed ({} request(s) without `{name}`, 0 uses); the allowed run hit                  this key's Google Search grounding quota: {}",
                denied.requests.len(),
                brief(error)
            )));
        }
        return Ok(Verdict::Fail(format!("allowed run failed: {}", brief(error))));
    }
    if !allowed.requests.iter().all(|tools| tools.contains(&name)) {
        return Ok(Verdict::Fail(format!(
            "allowed run requests carried tools {:?}",
            allowed.requests
        )));
    }
    if allowed.uses == 0 {
        return Ok(Verdict::Retry("model answered without searching".to_string()));
    }
    let catalog = PricingCatalog::standard();
    let mut fees = 0.0;
    for usage in allowed.usages.iter().filter(|usage| !usage.builtin_tool_uses.is_empty()) {
        let Some(cost) = usage.cost else {
            return Ok(Verdict::Fail(format!("{} usage without a cost", setup.model_id)));
        };
        let tokens_only = UsageMetadata { builtin_tool_uses: Vec::new(), ..usage.clone() };
        let token_cost =
            catalog.cost_usd(Some(setup.vendor), &setup.model_id, &tokens_only).unwrap_or(0.0);
        let fee = catalog.builtin_tool_cost_usd(Some(setup.vendor), &setup.model_id, usage);
        if (cost - token_cost - fee).abs() > 1e-9 || fee <= 0.0 {
            return Ok(Verdict::Fail(format!(
                "cost {cost} is not tokens {token_cost} + fee {fee} for {:?}",
                usage.builtin_tool_uses
            )));
        }
        fees += fee;
    }
    let kinds: Vec<String> = allowed
        .usages
        .iter()
        .flat_map(|usage| {
            usage.builtin_tool_uses.iter().map(|used| format!("{}×{}", used.kind, used.count))
        })
        .collect();
    Ok(Verdict::Pass(format!(
        "{}: denied → {} request(s) without `{name}`, 0 uses, withheld policyDenied; allowed → {} \
         use(s) [{}], fees ${fees:.4} in cost",
        setup.model_id,
        denied.requests.len(),
        allowed.uses,
        kinds.join(", ")
    )))
}
