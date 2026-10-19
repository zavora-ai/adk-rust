//! Item 4: notification and RSS action nodes follow the executor's `HttpActionPolicy`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_action::{
    ActionNodeConfig, Callbacks, ErrorHandling, ErrorMode, ExecutionControl, InputOutputMapping,
    LogLevel, NotificationChannel, NotificationMessage, NotificationNodeConfig, RssNodeConfig,
    StandardProperties, Tracing,
};
use adk_graph::action::ActionNodeExecutor;
use adk_graph::action::http::HttpActionPolicy;
use adk_graph::node::{ExecutionConfig, Node, NodeContext};
use adk_graph::state::State;
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};

use crate::common::{Mode, Report, Verdict, verdict};

const FEED: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
<item><title>Internal</title><link>http://internal/1</link><description>secret</description></item>
</channel></rss>"#;

fn standard(id: &str) -> StandardProperties {
    StandardProperties {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: false, log_level: LogLevel::None },
        callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: ExecutionControl { timeout: 10_000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: "result".to_string() },
    }
}

/// A local server standing in for an internal service; counts every request it receives.
struct Internal {
    base: String,
    hits: Arc<AtomicUsize>,
}

async fn internal() -> anyhow::Result<Internal> {
    let hits = Arc::new(AtomicUsize::new(0));
    let (h1, h2) = (hits.clone(), hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let app = Router::new()
        .route(
            "/hook",
            post(move || async move {
                h1.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }),
        )
        .route(
            "/feed",
            get(move || async move {
                h2.fetch_add(1, Ordering::SeqCst);
                ([(header::CONTENT_TYPE, "application/rss+xml")], FEED).into_response()
            }),
        )
        .route(
            "/redirect",
            get(move || async move {
                // Sends the reader to a host the policy does not list.
                (
                    StatusCode::FOUND,
                    [(header::LOCATION, format!("http://localhost:{port}/feed"))],
                )
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Internal { base: format!("http://127.0.0.1:{port}"), hits })
}

fn notification(url: &str) -> ActionNodeConfig {
    ActionNodeConfig::Notification(NotificationNodeConfig {
        standard: standard("notify"),
        notification_channel: NotificationChannel::Webhook,
        webhook_url: url.to_string(),
        message: NotificationMessage {
            text: "deploy finished".to_string(),
            format: None,
            username: None,
            icon_url: None,
            channel: None,
        },
    })
}

fn rss(url: &str) -> ActionNodeConfig {
    ActionNodeConfig::Rss(RssNodeConfig {
        standard: standard("feed"),
        feed_url: url.to_string(),
        filter: None,
        seen_tracking: None,
    })
}

/// Runs the node once; `Err` carries the failure message.
async fn execute(executor: &ActionNodeExecutor) -> Result<(), String> {
    let ctx = NodeContext::new(State::new(), ExecutionConfig::new("release-tool-fixes"), 0);
    executor.execute(&ctx).await.map(|_| ()).map_err(|error| error.to_string())
}

pub async fn run(report: &mut Report) {
    report.record_result("notification-host-allowlist", 4, Mode::Local, notification_host().await);
    report.record_result("notification-scheme-policy", 4, Mode::Local, notification_scheme().await);
    report.record_result("rss-host-allowlist", 4, Mode::Local, rss_host().await);
    report.record_result("rss-redirect-checked", 4, Mode::Local, rss_redirect().await);
}

/// A webhook URL outside the host allowlist is refused before any request is sent.
async fn notification_host() -> anyhow::Result<Verdict> {
    let server = internal().await?;
    let executor = ActionNodeExecutor::new(notification(&format!("{}/hook", server.base)))
        .with_http_policy(HttpActionPolicy::new().allow_hosts(["hooks.slack.com"]));
    let outcome = execute(&executor).await;
    let hits = server.hits.load(Ordering::SeqCst);
    Ok(verdict(vec![
        (outcome.is_err(), format!("node outcome: {outcome:?}")),
        (hits == 0, format!("internal service received {hits} request(s)")),
    ]))
}

/// An `http` webhook is refused by a policy that permits only `https`.
async fn notification_scheme() -> anyhow::Result<Verdict> {
    let server = internal().await?;
    let executor = ActionNodeExecutor::new(notification(&format!("{}/hook", server.base)))
        .with_http_policy(HttpActionPolicy::new().allow_schemes(["https"]));
    let outcome = execute(&executor).await;
    let hits = server.hits.load(Ordering::SeqCst);
    Ok(verdict(vec![
        (outcome.is_err(), format!("node outcome: {outcome:?}")),
        (hits == 0, format!("internal service received {hits} request(s)")),
    ]))
}

/// A feed URL outside the host allowlist is refused before any request is sent.
async fn rss_host() -> anyhow::Result<Verdict> {
    let server = internal().await?;
    let executor = ActionNodeExecutor::new(rss(&format!("{}/feed", server.base)))
        .with_http_policy(HttpActionPolicy::new().allow_hosts(["feeds.example.com"]));
    let outcome = execute(&executor).await;
    let hits = server.hits.load(Ordering::SeqCst);
    Ok(verdict(vec![
        (outcome.is_err(), format!("node outcome: {outcome:?}")),
        (hits == 0, format!("internal service received {hits} request(s)")),
    ]))
}

/// A redirect to a host the policy does not list is not followed.
async fn rss_redirect() -> anyhow::Result<Verdict> {
    let server = internal().await?;
    let executor = ActionNodeExecutor::new(rss(&format!("{}/redirect", server.base)))
        .with_http_policy(HttpActionPolicy::new().allow_hosts(["127.0.0.1"]));
    let outcome = execute(&executor).await;
    let hits = server.hits.load(Ordering::SeqCst);
    Ok(verdict(vec![
        (outcome.is_err(), format!("node outcome: {outcome:?}")),
        (hits == 0, format!("redirect target received {hits} request(s)")),
    ]))
}
