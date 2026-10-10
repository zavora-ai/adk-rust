//! Anthropic-only scenarios: web search `caller` round-trip (#754, #759) and 1-hour prompt cache
//! pricing (#735, #759).

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_anthropic::Usage;
use adk_anthropic::pricing::{ModelPricing, estimate_cost};
use adk_core::{Agent, Content, Llm, LlmRequest, Tool};
use adk_model::anthropic::CacheTtl;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::WebSearchTool;
use futures::StreamExt;

use crate::common::{Config, Provider, Verdict, brief, fail, run_turn, session_events};

const USER: &str = "user-anthropic";

// ─── anthropic_web_tools ────────────────────────────────────────────────────

/// Whether an Anthropic error says web search is unavailable to this organization.
fn web_search_unavailable(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("web search") && (error.contains("not enabled") || error.contains("disabled"))
        || error.contains("web_search") && error.contains("not available")
}

async fn web_variant(model: Arc<dyn Llm>, direct: bool) -> anyhow::Result<Verdict> {
    let mut tool = WebSearchTool::new().with_dynamic_filtering().with_max_uses(2);
    if direct {
        tool = tool.with_allowed_callers(["direct"]);
    }
    let agent = LlmAgentBuilder::new("researcher")
        .model(model)
        .instruction("Use web_search for questions about current events. Answer in one sentence.")
        .tool(Arc::new(tool) as Arc<dyn Tool>)
        .build()?;
    let app = "autonomy-web";
    let session_id = if direct { "web-direct" } else { "web-dynamic" };
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
        .build()?;

    let first = run_turn(
        &runner,
        USER,
        session_id,
        "Search the web for one news headline published in the last two days and quote it.",
    )
    .await;
    if let Some(error) = &first.error {
        return Ok(if web_search_unavailable(error) {
            Verdict::Skip(format!("web search unavailable: {}", brief(error)))
        } else {
            Verdict::Fail(format!("turn 1 failed: {}", brief(error)))
        });
    }
    let events = session_events(sessions.as_ref(), app, USER, session_id).await?;
    let history = serde_json::to_string(&events)?;
    let searches = history.matches("\"server_tool_use\"").count();
    if searches == 0 {
        return Ok(Verdict::Retry("model answered without a web search".to_string()));
    }
    let callers = history.matches("\"caller\"").count();

    let second = run_turn(
        &runner,
        USER,
        session_id,
        "Thanks. In at most five words, what topic was that headline about?",
    )
    .await;
    if let Some(error) = &second.error {
        return Ok(Verdict::Fail(format!(
            "turn 2 replaying the search history was rejected ({searches} server_tool_use blocks, {callers} caller fields): {}",
            brief(error)
        )));
    }
    Ok(Verdict::Pass(format!(
        "{}: turn 2 accepted replaying {searches} server_tool_use block(s), {callers} `caller` field(s)",
        if direct { "allowed_callers=[direct]" } else { "dynamic filtering" }
    )))
}

pub async fn web_tools(cfg: &Config, provider: Provider) -> Verdict {
    if provider != Provider::Anthropic {
        return Verdict::Skip("Anthropic server tool".to_string());
    }
    let model = match cfg.anthropic(&cfg.anthropic_web_model, |config| config) {
        Ok(model) => model as Arc<dyn Llm>,
        Err(error) => return fail("model setup", error),
    };
    let mut evidence = Vec::new();
    for direct in [true, false] {
        match web_variant(Arc::clone(&model), direct).await {
            Ok(Verdict::Pass(detail)) => evidence.push(detail),
            Ok(other) => return other,
            Err(error) => return fail("setup", error),
        }
    }
    Verdict::Pass(format!("{} ({})", evidence.join("; "), cfg.anthropic_web_model))
}

// ─── anthropic_cache_cost ───────────────────────────────────────────────────

/// A system prompt well above every current model's minimum cacheable prefix.
fn long_system_prompt(marker: &str) -> String {
    let regions = ["EMEA", "APAC", "LATAM", "NA", "Africa"];
    let clauses: Vec<String> = (1..=120)
        .map(|i| {
            format!(
                "Clause {i}: agents in region {} verify the customer's identity, record the ticket \
                 number, quote the refund policy section {i}, and never disclose internal \
                 escalation codes.",
                regions[i % regions.len()]
            )
        })
        .collect();
    format!("Support policy, revision {marker}.\n{}", clauses.join("\n"))
}

async fn call_usage(model: &dyn Llm, system: &str) -> anyhow::Result<Usage> {
    let request = LlmRequest::new(
        model.name(),
        vec![
            Content::new("system").with_text(system),
            Content::new("user").with_text("Reply with the single word OK."),
        ],
    );
    let mut stream = model.generate_content(request, false).await?;
    let mut usage = None;
    while let Some(response) = stream.next().await {
        let response = response?;
        if let Some(provider_usage) =
            response.usage_metadata.and_then(|metadata| metadata.provider_usage)
        {
            usage = Some(serde_json::from_value::<Usage>(provider_usage)?);
        }
    }
    usage.ok_or_else(|| anyhow::anyhow!("response carried no Anthropic usage"))
}

pub async fn cache_cost(cfg: &Config, provider: Provider) -> Verdict {
    if provider != Provider::Anthropic {
        return Verdict::Skip("Anthropic prompt caching".to_string());
    }
    match cache_scenario(cfg).await {
        Ok(verdict) => verdict,
        Err(error) => fail("call", error),
    }
}

async fn cache_scenario(cfg: &Config) -> anyhow::Result<Verdict> {
    let model_id = cfg.anthropic_model.clone();
    let model = cfg.anthropic(&model_id, |config| {
        config
            .with_prompt_caching(true)
            .with_prompt_cache_ttl(CacheTtl::one_hour())
            .with_max_tokens(1024)
    })?;
    // A fresh marker so an entry left by an earlier run within the hour cannot be read.
    let marker = format!(
        "{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
    );
    let system = long_system_prompt(&marker);
    let first = call_usage(model.as_ref(), &system).await?;
    let second = call_usage(model.as_ref(), &system).await?;

    let written = first.cache_creation_input_tokens.unwrap_or_default();
    let written_1h = first.cache_creation_input_tokens_1h.unwrap_or_default();
    let read = second.cache_read_input_tokens.unwrap_or_default();
    let Some(pricing) = ModelPricing::for_model_id(&model_id) else {
        return Ok(Verdict::Fail(format!("no pricing for {model_id}")));
    };
    let cost = estimate_cost(pricing, &first);
    let expected_write = written_1h as f64 / 1_000_000.0 * pricing.cache_write_1h;
    let write_multiplier = if written > 0 {
        cost.cache_write_cost / (written as f64 / 1_000_000.0 * pricing.input)
    } else {
        0.0
    };

    let mut issues = Vec::new();
    if written == 0 {
        issues.push(format!("call 1 wrote no cache entry (usage {first:?})"));
    }
    if written_1h != written {
        issues.push(format!("call 1 wrote {written} tokens but only {written_1h} at the 1h TTL"));
    }
    if read == 0 {
        issues.push(format!("call 2 read nothing from the cache (usage {second:?})"));
    }
    if (pricing.cache_write_1h - 2.0 * pricing.input).abs() > 1e-9 {
        issues.push(format!(
            "{model_id} prices 1h writes at {} vs base input {}",
            pricing.cache_write_1h, pricing.input
        ));
    }
    if (cost.cache_write_cost - expected_write).abs() > 1e-12
        || (write_multiplier - 2.0).abs() > 1e-6
    {
        issues.push(format!(
            "estimate_cost billed the 1h write at ${:.8} ({write_multiplier:.3}x base), expected ${expected_write:.8} (2x)",
            cost.cache_write_cost
        ));
    }
    Ok(if issues.is_empty() {
        Verdict::Pass(format!(
            "{model_id}: call 1 wrote {written} tokens, all 1h; call 2 read {read}; estimate_cost bills the write at {write_multiplier:.2}x base input (${:.8})",
            cost.cache_write_cost
        ))
    } else {
        Verdict::Fail(issues.join("; "))
    })
}
