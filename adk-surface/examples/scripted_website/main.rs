//! The website's views on the web home over a bakery's site (spec 3.5 §8.2): what the
//! website end-to-end tests in `adk-surface/web/e2e` open, and what the screenshots show.
//!
//! Without `--live` the site is fresh: it prints a setup code and writes it to
//! `<workspace>/setup-code`, and accepts no onboarding input until a device claims it
//! (spec 3.5 A-I20). Discovery reads a recorded online presence; three services can be
//! connected; the launch publishes a signed release to `<workspace>/host`. With `--live` the
//! site has been running for a week: releases, KPIs, jobs with an earned promotion, signals,
//! a wallet, held updates, an alarm and last week's review.
//!
//! The server is `adk-server`'s `ServerBuilder` with the engine and the desk under
//! `/api/surface`, the setup routes as root routes and the page at `/ui/`, as `adk-rust up`
//! mounts them; a stand-in extractor admits every request as the owner, as a loopback-only
//! server's does.
//!
//! ```sh
//! cargo run -p adk-surface --features website --example scripted_website -- 8801 /tmp/website [--live]
//! ```

mod site;

use std::path::PathBuf;
use std::sync::Arc;

use adk_core::{InMemoryApprovalStore, RequestContext, SingleAgentLoader, StoreApprovalService};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::{LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;

/// Admits every request as the owner, as a loopback-only server's extractor does.
struct Owner;

#[async_trait]
impl RequestContextExtractor for Owner {
    async fn extract(
        &self,
        _parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        Ok(RequestContext {
            user_id: site::OWNER.into(),
            scopes: vec!["owner".into(), "operator".into()],
            metadata: Default::default(),
        })
    }
}

/// The agent the engine's roster lists: the site's Steward, which takes no turn here.
struct Steward;

#[async_trait]
impl adk_core::Agent for Steward {
    fn name(&self) -> &str {
        "steward"
    }

    fn description(&self) -> &str {
        "Leads the bakery's site"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8801), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    let live = args.any(|arg| arg == "--live");
    std::fs::create_dir_all(&workspace)?;

    let site = if live {
        site::live(&workspace).await
    } else {
        site::fresh(&workspace, awp_types::SiteKind::Business).await
    };
    if let Some(code) = &site.setup_code {
        std::fs::write(workspace.join("setup-code"), code)?;
        println!("setup code: {code}");
    }

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let source = LocalSource::builder("juniper")
        .sessions(sessions.clone())
        .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source)).places([Place::web()]).build();
    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(Steward))), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_api_routes(adk_surface::web::website::router(site.desk.clone()))
        .add_root_routes(adk_surface::web::website::setup_router(site.desk.clone()))
        .add_root_routes(adk_surface::web::home())
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("scripted website on http://127.0.0.1:{port}/ui/website");
    axum::serve(listener, app).await?;
    Ok(())
}
