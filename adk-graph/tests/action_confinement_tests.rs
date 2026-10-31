//! Action nodes take paths and URLs from workflow state, which is untrusted.
//!
//! A file node used the interpolated path as given, so `../` or an absolute
//! path reached any file the process could. The HTTP, notification, and RSS
//! nodes requested any URL the state produced. All are now confined — the HTTP
//! nodes to public addresses unless the host opts in — and a node whose backend does
//! not exist says it is not implemented rather than pointing at a feature flag.

#![cfg(feature = "action")]

use std::path::PathBuf;

use adk_action::{
    ActionNodeConfig, Callbacks, CodeLanguage, CodeNodeConfig, DatabaseConnection,
    DatabaseNodeConfig, DatabaseType, ErrorHandling, ErrorMode, ExecutionControl, FileNodeConfig,
    FileOperation, FileWriteConfig, InputOutputMapping, LocalFileConfig, LogLevel, SqlConfig,
    StandardProperties, Tracing,
};
use adk_graph::action::ActionNodeExecutor;
use adk_graph::error::GraphError;
use adk_graph::node::{ExecutionConfig, Node, NodeContext};
use adk_graph::state::State;
use serde_json::json;

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
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: "result".to_string() },
    }
}

/// Runs `executor` once against `state`, returning the failure message.
async fn failure(executor: &ActionNodeExecutor, state: State) -> String {
    let ctx = NodeContext::new(state, ExecutionConfig::new("confinement"), 0);
    match executor.execute(&ctx).await {
        Err(GraphError::NodeExecutionFailed { message, .. }) => message,
        Err(other) => panic!("expected a node failure, got {other}"),
        Ok(_) => panic!("expected a node failure, but the node succeeded"),
    }
}

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("adk-graph-action-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_node(path_template: &str) -> ActionNodeConfig {
    ActionNodeConfig::File(FileNodeConfig {
        standard: standard("write"),
        operation: FileOperation::Write,
        local: Some(LocalFileConfig { path: path_template.to_string() }),
        cloud: None,
        parse: None,
        write: Some(FileWriteConfig {
            content: json!("payload"),
            create_dirs: true,
            append: false,
        }),
        list: None,
    })
}

#[tokio::test]
async fn a_file_path_escaping_the_root_through_parent_components_is_rejected() {
    let workspace = TempDir::new();
    let root = workspace.0.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let executor = ActionNodeExecutor::new(write_node("{{target}}")).with_file_roots([&root]);

    let state = State::from([("target".to_string(), json!("../escaped.txt"))]);
    let message = failure(&executor, state).await;

    assert!(message.contains("contains '..'"), "{message}");
    assert!(!workspace.0.join("escaped.txt").exists(), "nothing may be written outside the root");
}

#[tokio::test]
async fn a_file_path_inside_the_root_is_written() {
    let root = TempDir::new();
    let executor = ActionNodeExecutor::new(write_node("{{target}}")).with_file_roots([&root.0]);

    let state = State::from([("target".to_string(), json!("nested/out.txt"))]);
    let ctx = NodeContext::new(state, ExecutionConfig::new("confinement"), 0);
    executor.execute(&ctx).await.expect("a path inside the root is permitted");

    assert_eq!(std::fs::read_to_string(root.0.join("nested/out.txt")).unwrap(), "payload");
}

#[cfg(feature = "action-http")]
#[tokio::test]
async fn a_file_url_is_rejected_by_an_http_node() {
    use adk_action::{HttpAuth, HttpBody, HttpMethod, HttpNodeConfig, HttpResponse};
    use std::collections::HashMap;

    let executor = ActionNodeExecutor::new(ActionNodeConfig::Http(HttpNodeConfig {
        standard: standard("fetch"),
        method: HttpMethod::Get,
        url: "{{url}}".to_string(),
        auth: HttpAuth::None,
        headers: HashMap::new(),
        body: HttpBody::None,
        response: HttpResponse { response_type: "text".to_string(), status_validation: None },
        rate_limit: None,
    }));

    let state = State::from([("url".to_string(), json!("file:///etc/passwd"))]);
    let message = failure(&executor, state).await;

    assert!(message.contains("scheme 'file' is not permitted"), "{message}");
}

#[cfg(feature = "action-http")]
fn fetch_node() -> ActionNodeConfig {
    use adk_action::{HttpAuth, HttpBody, HttpMethod, HttpNodeConfig, HttpResponse};

    ActionNodeConfig::Http(HttpNodeConfig {
        standard: standard("fetch"),
        method: HttpMethod::Get,
        url: "{{url}}".to_string(),
        auth: HttpAuth::None,
        headers: std::collections::HashMap::new(),
        body: HttpBody::None,
        response: HttpResponse { response_type: "text".to_string(), status_validation: None },
        rate_limit: None,
    })
}

/// A URL from workflow state reaches no loopback, private, link-local or metadata address,
/// and no name for one, under the default policy.
#[cfg(feature = "action-http")]
#[tokio::test]
async fn an_http_node_reaches_no_internal_address_by_default() {
    let internal = Internal::start().await;
    let port = internal.base.rsplit(':').next().unwrap().to_string();
    let executor = ActionNodeExecutor::new(fetch_node());
    for url in [
        format!("{}/status", internal.base),
        format!("http://localhost:{port}/status"),
        format!("http://[::ffff:127.0.0.1]:{port}/status"),
        format!("http://2130706433:{port}/status"),
        "http://169.254.169.254/latest/meta-data/iam/security-credentials/".to_string(),
        "http://10.0.0.7/admin".to_string(),
        "https://[fd00:ec2::254]/latest/meta-data/".to_string(),
    ] {
        let state = State::from([("url".to_string(), json!(url))]);
        let message = failure(&executor, state).await;
        assert!(message.contains("non-public address"), "{url}: {message}");
    }
    assert_eq!(internal.served(), 0, "no internal request was sent");
}

/// `allow_private_networks` admits internal targets, by address and by a name that
/// resolves to one.
#[cfg(feature = "action-http")]
#[tokio::test]
async fn private_targets_are_reached_only_with_the_opt_in() {
    use adk_graph::action::http::HttpActionPolicy;

    let internal = Internal::start().await;
    let port = internal.base.rsplit(':').next().unwrap().to_string();
    let executor = ActionNodeExecutor::new(fetch_node())
        .with_http_policy(HttpActionPolicy::new().allow_private_networks());
    for url in [format!("{}/status", internal.base), format!("http://localhost:{port}/status")] {
        let state = State::from([("url".to_string(), json!(url))]);
        let ctx = NodeContext::new(state, ExecutionConfig::new("confinement"), 0);
        executor.execute(&ctx).await.unwrap_or_else(|error| panic!("{url}: {error}"));
    }
    assert_eq!(internal.served(), 2);
}

/// A local HTTP server standing in for an internal service.
///
/// It answers `GET /redirect` with a redirect to `localhost` on the same port, and every
/// other request with an empty `200`, counting the requests it answers that way.
#[cfg(feature = "action-http")]
struct Internal {
    base: String,
    served: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "action-http")]
impl Internal {
    async fn start() -> Self {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = served.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = vec![0u8; 4096];
                let read = stream.read(&mut request).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&request[..read]).to_string();
                let response = if head.starts_with("GET /redirect ") {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/feed\r\n\
                         Content-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    counter.fetch_add(1, Ordering::SeqCst);
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self { base: format!("http://127.0.0.1:{port}"), served }
    }

    fn served(&self) -> usize {
        self.served.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(feature = "action-http")]
fn notification_node(url: &str) -> ActionNodeConfig {
    use adk_action::{NotificationChannel, NotificationMessage, NotificationNodeConfig};

    ActionNodeConfig::Notification(NotificationNodeConfig {
        standard: standard("notify"),
        notification_channel: NotificationChannel::Slack,
        webhook_url: url.to_string(),
        message: NotificationMessage {
            text: "deployed".to_string(),
            format: None,
            username: None,
            icon_url: None,
            channel: None,
        },
    })
}

#[cfg(feature = "action-http")]
#[tokio::test]
async fn a_notification_webhook_outside_the_host_allowlist_is_never_requested() {
    use adk_graph::action::http::HttpActionPolicy;

    let internal = Internal::start().await;
    let executor = ActionNodeExecutor::new(notification_node("{{hook}}"))
        .with_http_policy(HttpActionPolicy::new().allow_hosts(["hooks.slack.com"]));
    let state = State::from([(
        "hook".to_string(),
        json!(format!("{}/services/T0/B0/webhook-secret", internal.base)),
    )]);

    let message = failure(&executor, state).await;

    assert!(message.contains("not in the HTTP action policy allowlist"), "{message}");
    assert!(!message.contains("webhook-secret"), "the webhook URL is a secret: {message}");
    assert_eq!(internal.served(), 0);
}

#[cfg(feature = "action-http")]
#[tokio::test]
async fn a_notification_webhook_with_a_refused_scheme_is_never_requested() {
    use adk_graph::action::http::HttpActionPolicy;

    let internal = Internal::start().await;
    let executor = ActionNodeExecutor::new(notification_node(&format!("{}/hook", internal.base)))
        .with_http_policy(HttpActionPolicy::new().allow_schemes(["https"]));

    let message = failure(&executor, State::new()).await;

    assert!(message.contains("scheme 'http' is not permitted"), "{message}");
    assert_eq!(internal.served(), 0);
}

#[cfg(feature = "action-http")]
#[tokio::test]
async fn a_notification_webhook_the_policy_permits_is_sent() {
    use adk_graph::action::http::HttpActionPolicy;

    let internal = Internal::start().await;
    let executor = ActionNodeExecutor::new(notification_node(&format!("{}/hook", internal.base)))
        .with_http_policy(
            HttpActionPolicy::new().allow_hosts(["127.0.0.1"]).allow_private_networks(),
        );
    let ctx = NodeContext::new(State::new(), ExecutionConfig::new("confinement"), 0);

    executor.execute(&ctx).await.expect("a permitted webhook is sent");

    assert_eq!(internal.served(), 1);
}

#[cfg(feature = "action-rss")]
fn rss_node(url: &str) -> ActionNodeConfig {
    ActionNodeConfig::Rss(adk_action::RssNodeConfig {
        standard: standard("feed"),
        feed_url: url.to_string(),
        filter: None,
        seen_tracking: None,
    })
}

#[cfg(feature = "action-rss")]
#[tokio::test]
async fn an_rss_feed_outside_the_host_allowlist_is_never_requested() {
    use adk_graph::action::http::HttpActionPolicy;

    let internal = Internal::start().await;
    let executor = ActionNodeExecutor::new(rss_node(&format!("{}/feed", internal.base)))
        .with_http_policy(HttpActionPolicy::new().allow_hosts(["feeds.example.com"]));

    let message = failure(&executor, State::new()).await;

    assert!(message.contains("not in the HTTP action policy allowlist"), "{message}");
    assert_eq!(internal.served(), 0);
}

#[cfg(feature = "action-rss")]
#[tokio::test]
async fn an_rss_redirect_to_an_unlisted_host_is_not_followed() {
    use adk_graph::action::http::HttpActionPolicy;

    let internal = Internal::start().await;
    let executor = ActionNodeExecutor::new(rss_node(&format!("{}/redirect", internal.base)))
        .with_http_policy(
            HttpActionPolicy::new().allow_hosts(["127.0.0.1"]).allow_private_networks(),
        );

    let message = failure(&executor, State::new()).await;

    assert!(message.contains("failed to fetch feed"), "{message}");
    assert_eq!(internal.served(), 0, "the redirect target was requested");
}

#[tokio::test]
async fn unimplemented_nodes_say_so_when_executed() {
    let database = ActionNodeExecutor::new(ActionNodeConfig::Database(DatabaseNodeConfig {
        standard: standard("db"),
        connection: DatabaseConnection {
            database_type: DatabaseType::Postgresql,
            connection_string: Some("postgres://localhost/db".to_string()),
            credential_ref: None,
        },
        sql: Some(SqlConfig {
            query: "select 1".to_string(),
            params: Vec::new(),
            operation: "query".to_string(),
        }),
        mongo: None,
        redis: None,
    }));
    let javascript = ActionNodeExecutor::new(ActionNodeConfig::Code(CodeNodeConfig {
        standard: standard("js"),
        language: CodeLanguage::Javascript,
        code: "return 1;".to_string(),
        sandbox: None,
    }));

    for message in
        [failure(&database, State::new()).await, failure(&javascript, State::new()).await]
    {
        assert!(message.contains("not implemented"), "{message}");
        assert!(!message.contains("feature is reserved"), "{message}");
    }
}
