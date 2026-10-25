//! The tray's reads (spec 3.3 §5.5, §7.2): the agent its "Ask Finance" box addresses, named
//! before that agent has ever run, and the tray row's answers over HTTP.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;

use adk_core::RequestContext;
use adk_kernel::control::ControlPlaneConfig;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::InMemorySessionService;
use adk_surface::{LocalSource, SurfaceEngine};
use common::{OWNER, call, scenario, scenario_with};
use serde_json::json;

fn model() -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("Done.")]))
}

#[tokio::test]
async fn the_agents_read_names_the_kernel_target_as_primary_before_it_ever_ran() {
    let scenario = scenario(model()).await;
    let api = common::api(&scenario.kernel, &scenario.engine);
    let (status, body) = call(&api, "GET", "/api/surface/agents?surface=tray", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["agents"], json!([]));
    assert_eq!(body["primary"]["name"], "home");
    assert_eq!(body["primary"]["displayName"], "Finance");
    assert_eq!(
        scenario.engine.primary_agent().map(|agent| agent.display_name),
        Some("Finance".into())
    );
    scenario.kernel.stop().await.unwrap();
}

#[tokio::test]
async fn a_harness_without_a_kernel_is_its_own_primary() {
    let scenario =
        scenario_with(model(), ControlPlaneConfig::default().without_socket(), false).await;
    assert_eq!(scenario.engine.primary_agent().map(|agent| agent.name), Some("finance".into()));
}

#[tokio::test]
async fn a_source_with_no_single_agent_names_no_primary() {
    let source = LocalSource::builder("plain")
        .sessions(Arc::new(InMemorySessionService::new()))
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    assert_eq!(engine.primary_agent(), None);
    let api = axum::Router::new().nest("/api", adk_surface::web::router(engine)).layer(
        axum::Extension(Some(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into()],
            metadata: Default::default(),
        })),
    );
    let (status, body) = call(&api, "GET", "/api/surface/agents?surface=tray", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({ "agents": [] }));
}
