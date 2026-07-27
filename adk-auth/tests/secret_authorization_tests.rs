//! Secret access from tools must be authorized and recorded.
//!
//! `SecretService` and `SecretProvider` received only a secret *name*. Once a provider
//! was attached to an invocation, policy collapsed to whatever the backing cloud
//! credentials could read: nothing distinguished a weather tool asking for its own API
//! key from the same tool asking for a payment credential, and no record of the access
//! existed at the ADK layer.

use adk_auth::secrets::authorizing::{
    AuthorizingSecretService, SecretAccessDecision, SecretAuditSink, SecretGrant,
};
use adk_core::{AdkError, SecretRequest, SecretService};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

/// An inner service that records which names it was actually asked for.
struct RecordingService {
    asked: Arc<Mutex<Vec<String>>>,
}

impl RecordingService {
    fn new() -> (Arc<Self>, Arc<Mutex<Vec<String>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        (Arc::new(Self { asked: asked.clone() }), asked)
    }
}

#[async_trait]
impl SecretService for RecordingService {
    async fn get_secret(&self, name: &str) -> adk_core::Result<String> {
        self.asked.lock().unwrap().push(name.to_string());
        Ok(format!("value-of-{name}"))
    }
}

/// One captured decision: allowed, secret name, tool, reason.
type CapturedDecision = (bool, String, Option<String>, &'static str);

/// Captures decisions so the audit record can be inspected.
#[derive(Default)]
struct CapturingSink {
    records: Mutex<Vec<CapturedDecision>>,
}

impl SecretAuditSink for CapturingSink {
    fn record(&self, decision: SecretAccessDecision<'_>) {
        self.records.lock().unwrap().push((
            decision.allowed,
            decision.name.to_string(),
            decision.tool_name.map(str::to_string),
            decision.reason,
        ));
    }
}

fn request(tool: &str, name: &str) -> SecretRequest {
    SecretRequest::new(name)
        .with_tool_name(tool)
        .with_identity("app", "user-1", "session-1")
        .with_invocation_id("inv-1")
}

fn service(inner: Arc<RecordingService>) -> AuthorizingSecretService {
    AuthorizingSecretService::new(inner)
        .grant("weather_lookup", SecretGrant::none().name("weather-api-key"))
        .grant("charge_card", SecretGrant::none().prefix("billing/"))
}

#[tokio::test]
async fn a_tool_can_read_only_its_granted_secret() {
    let (inner, asked) = RecordingService::new();
    let service = service(inner);

    let allowed = service.get_secret_for(&request("weather_lookup", "weather-api-key")).await;
    assert_eq!(allowed.unwrap(), "value-of-weather-api-key");
    assert_eq!(asked.lock().unwrap().as_slice(), ["weather-api-key"]);
}

#[tokio::test]
async fn a_denied_name_never_reaches_the_provider() {
    let (inner, asked) = RecordingService::new();
    let service = service(inner);

    let denied = service.get_secret_for(&request("weather_lookup", "billing/stripe-key")).await;

    assert!(denied.is_err(), "a tool read a secret outside its grant");
    assert!(
        asked.lock().unwrap().is_empty(),
        "the denied name was still looked up in the provider: {:?}",
        asked.lock().unwrap()
    );
}

#[tokio::test]
async fn a_prefix_grant_covers_its_namespace_and_nothing_else() {
    let (inner, _asked) = RecordingService::new();
    let service = service(inner);

    assert!(service.get_secret_for(&request("charge_card", "billing/stripe-key")).await.is_ok());
    assert!(
        service.get_secret_for(&request("charge_card", "weather-api-key")).await.is_err(),
        "a prefix grant must not reach outside its namespace"
    );
}

#[tokio::test]
async fn an_ungranted_tool_is_denied() {
    let (inner, asked) = RecordingService::new();
    let service = service(inner);

    let denied = service.get_secret_for(&request("unknown_tool", "weather-api-key")).await;
    assert!(denied.is_err(), "a tool with no grant must be denied");
    assert!(asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_request_without_identity_is_denied() {
    // A bare name cannot be attributed, so there is nothing to authorize against.
    let (inner, asked) = RecordingService::new();
    let service = service(inner);

    assert!(service.get_secret("weather-api-key").await.is_err());
    assert!(
        service.get_secret_for(&SecretRequest::new("weather-api-key")).await.is_err(),
        "an access with no tool identity must be denied by default"
    );
    assert!(asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_untooled_grant_can_be_opened_deliberately() {
    let (inner, _asked) = RecordingService::new();
    let service =
        AuthorizingSecretService::new(inner).grant_untooled(SecretGrant::none().name("agent-key"));

    assert!(service.get_secret_for(&SecretRequest::new("agent-key")).await.is_ok());
    assert!(service.get_secret_for(&SecretRequest::new("other-key")).await.is_err());
}

#[tokio::test]
async fn a_tool_cannot_present_another_tools_grant() {
    // The framework sets `tool_name` from the tool it dispatched. This asserts the
    // policy side: a request naming a different tool only ever gets that tool's grant,
    // so claiming an identity cannot widen access beyond what was granted to it.
    let (inner, asked) = RecordingService::new();
    let service = service(inner);

    // `weather_lookup` claiming to be `charge_card` still cannot read a weather key,
    // because charge_card was never granted one.
    let spoofed = service.get_secret_for(&request("charge_card", "weather-api-key")).await;
    assert!(spoofed.is_err());
    assert!(asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn every_decision_is_audited_without_the_value() {
    let (inner, _asked) = RecordingService::new();
    let sink = Arc::new(CapturingSink::default());
    let service = service(inner).with_audit_sink(sink.clone());

    service.get_secret_for(&request("weather_lookup", "weather-api-key")).await.unwrap();
    let _ = service.get_secret_for(&request("weather_lookup", "billing/stripe-key")).await;

    let records = sink.records.lock().unwrap().clone();
    assert_eq!(records.len(), 2, "both the allow and the deny must be recorded");

    let (allowed, name, tool, _reason) = &records[0];
    assert!(*allowed);
    assert_eq!(name, "weather-api-key");
    assert_eq!(tool.as_deref(), Some("weather_lookup"));

    let (denied_allowed, denied_name, _, reason) = &records[1];
    assert!(!*denied_allowed);
    assert_eq!(denied_name, "billing/stripe-key");
    assert!(!reason.is_empty(), "a deny must say why");

    // The audit surface carries names and identities, never values.
    let rendered = format!("{records:?}");
    assert!(
        !rendered.contains("value-of-"),
        "the audit record contained a secret value: {rendered}"
    );
}

#[tokio::test]
async fn debug_output_lists_grants_without_values() {
    let (inner, _asked) = RecordingService::new();
    let rendered = format!("{:?}", service(inner));
    assert!(rendered.contains("weather_lookup"));
    assert!(!rendered.contains("value-of-"));
}

#[tokio::test]
async fn the_denial_error_is_unauthorized() {
    let (inner, _asked) = RecordingService::new();
    let service = service(inner);

    let error: AdkError =
        service.get_secret_for(&request("unknown_tool", "weather-api-key")).await.unwrap_err();
    assert!(error.is_unauthorized(), "a denial must be categorised as unauthorized: {error:?}");
}
