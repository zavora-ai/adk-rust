//! What the WhatsApp, Slack, Discord and Matrix adapter tests share (spec 3.5 §10 X12): a
//! gateway over the contract suite's kernel double, pairing, and waiting for what the fake
//! platform saw.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use adk_kernel::channels::{
    ChannelBinding, ChannelBindingStore, ChannelGateway, ChannelHost, ChannelSink,
    InMemoryChannelBindingStore,
};
use adk_kernel::conformance::ContractHost;
use chrono::Utc;

pub const HOME: &str = "https://home.example/ui/";
pub const WITHIN: Duration = Duration::from_secs(20);

/// Polls `probe` until it answers, for at most [`WITHIN`].
pub async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= WITHIN {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A gateway over `host` with `sink`, keeping pairings in `bindings`, started.
pub async fn gateway(
    host: Arc<dyn ChannelHost>,
    bindings: Arc<InMemoryChannelBindingStore>,
    sink: Arc<dyn ChannelSink>,
) -> ChannelGateway {
    let gateway = ChannelGateway::builder(host, bindings).sink(sink).home_url(HOME).build();
    gateway.start().await.unwrap();
    gateway
}

/// The contract suite's kernel double, a binding store and a gateway over them with `sink`.
pub async fn world(
    sink: Arc<dyn ChannelSink>,
) -> (Arc<ContractHost>, Arc<InMemoryChannelBindingStore>, ChannelGateway) {
    let host = Arc::new(ContractHost::new());
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    let gateway = gateway(host.clone(), bindings.clone(), sink).await;
    (host, bindings, gateway)
}

/// Pairs `account` on `channel`, whose cards go to `thread`, with `user` in the group the
/// contract suite's decisions are routed to.
pub async fn pair(
    bindings: &InMemoryChannelBindingStore,
    channel: &str,
    account: &str,
    thread: &str,
    user: &str,
) {
    let binding =
        ChannelBinding::new(channel, account, thread, user, Utc::now()).with_groups(["operator"]);
    bindings.put_binding(binding).await.unwrap();
}

/// The text after `adk-rust pair accept ` in `text`: a pairing code as the person reads it.
pub fn code_in(text: &str) -> Option<String> {
    let code = text.split("adk-rust pair accept ").nth(1)?;
    let code: String =
        code.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    (!code.is_empty()).then_some(code)
}

/// The values of `names` for a live test, from the environment or the repository's `.env`;
/// `None` unless every one is set.
pub fn live_env(names: &[&str]) -> Option<Vec<String>> {
    let env = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    if env.exists() {
        let _ = dotenvy::from_path_override(&env);
    }
    names.iter().map(|name| std::env::var(name).ok().filter(|value| !value.is_empty())).collect()
}

/// Captures every inbound value a gateway is given, and leaves each to it.
#[derive(Default)]
pub struct Capture(pub std::sync::Mutex<Vec<adk_kernel::channels::ChannelInbound>>);

#[async_trait::async_trait]
impl adk_kernel::channels::InboundRoute for Capture {
    async fn take(
        &self,
        inbound: &adk_kernel::channels::ChannelInbound,
        _binding: Option<&ChannelBinding>,
    ) -> adk_kernel::Result<bool> {
        self.0.lock().unwrap().push(inbound.clone());
        Ok(false)
    }
}

/// A gateway over the contract suite's kernel double with `sink`, whose route captures every
/// inbound value; `account` on `channel` is paired from `thread`.
pub async fn capturing(
    sink: Arc<dyn ChannelSink>,
    channel: &str,
    account: &str,
    thread: &str,
) -> (Arc<ContractHost>, Arc<Capture>, ChannelGateway) {
    let host = Arc::new(ContractHost::new());
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    pair(&bindings, channel, account, thread, "ana").await;
    let capture = Arc::new(Capture::default());
    let gateway = ChannelGateway::builder(host.clone(), bindings)
        .sink(sink)
        .home_url(HOME)
        .route(capture.clone())
        .build();
    gateway.start().await.unwrap();
    (host, capture, gateway)
}

/// Waits until `capture` holds `count` inbound values, and returns them.
pub async fn captured(
    capture: &Capture,
    count: usize,
) -> Vec<adk_kernel::channels::ChannelInbound> {
    eventually(&format!("{count} inbound values"), || async {
        let taken = capture.0.lock().unwrap().clone();
        (taken.len() >= count).then_some(taken)
    })
    .await
}
