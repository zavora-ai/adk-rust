//! AR21: `Runner::close` closes the runner's plugins and every agent's toolsets.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use adk_agent::LlmAgentBuilder;
use adk_core::{ReadonlyContext, Tool, Toolset, async_trait};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use serde_json::json;

use crate::scripted::{Reply, ScriptedServer, Turn};
use crate::{
    Counter, SESSION, Store, USER, Verdict, counter, gemini, google_key, lookup_tool, no_error,
    read, responses_for, run, scripted_model, verdict,
};

/// A toolset that counts how often it is closed.
struct CountingToolset {
    name: &'static str,
    tools: Vec<Arc<dyn Tool>>,
    closed: Counter,
}

#[async_trait]
impl Toolset for CountingToolset {
    fn name(&self) -> &str {
        self.name
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn Tool>>> {
        Ok(self.tools.clone())
    }

    async fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

fn toolset(name: &'static str, closed: Counter) -> Arc<dyn Toolset> {
    Arc::new(CountingToolset { name, tools: vec![lookup_tool()], closed })
}

fn closing_plugin(closed: Counter) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(vec![Plugin::new(PluginConfig {
        name: "metrics".to_string(),
        close_fn: Some(Box::new(move || {
            let closed = closed.clone();
            Box::pin(async move {
                closed.fetch_add(1, Ordering::SeqCst);
            })
        })),
        ..Default::default()
    })]))
}

pub(crate) async fn ar21_runner_close() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.tool_results {
        0 => Reply::call("lookup", json!({})),
        _ => Reply::text("it ships Monday"),
    }))
    .await?;
    let model = scripted_model(&server)?;
    let (desk_closed, helper_closed, plugin_closed) = (counter(), counter(), counter());
    let helper = LlmAgentBuilder::new("helper")
        .model(model.clone())
        .instruction("You are helper.")
        .toolset(toolset("helper-tools", helper_closed.clone()))
        .build()?;
    let desk = LlmAgentBuilder::new("desk")
        .model(model)
        .instruction("You are desk.")
        .toolset(toolset("desk-tools", desk_closed.clone()))
        .sub_agent(Arc::new(helper))
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), Some(closing_plugin(plugin_closed.clone())))?;
    let outcome = run(&runner, USER, SESSION, "where is A1?", None).await?;
    runner.close().await;
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            read(&plugin_closed) == 1,
            format!("the plugin is closed once ({})", read(&plugin_closed)),
        ),
        (
            read(&desk_closed) == 1 && read(&helper_closed) == 1,
            format!(
                "the root's and the sub-agent's toolsets are closed once each ({}, {})",
                read(&desk_closed),
                read(&helper_closed)
            ),
        ),
    ]))
}

pub(crate) async fn live_ar21_runner_close() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let (tools_closed, plugin_closed) = (counter(), counter());
    let desk = LlmAgentBuilder::new("desk")
        .model(gemini(&key)?)
        .instruction(
            "You are desk. Call lookup for the order, then state its status in one sentence.",
        )
        .toolset(toolset("desk-tools", tools_closed.clone()))
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), Some(closing_plugin(plugin_closed.clone())))?;
    let outcome = run(&runner, USER, SESSION, "Where is order A1?", None).await?;
    runner.close().await;
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            !responses_for(&outcome.events, "lookup").is_empty(),
            "Gemini called the toolset's lookup".to_string(),
        ),
        (
            read(&plugin_closed) == 1,
            format!("the plugin is closed once ({})", read(&plugin_closed)),
        ),
        (read(&tools_closed) == 1, format!("the toolset is closed once ({})", read(&tools_closed))),
    ]))
}
