//! A webhook that starts agent work needs a trust boundary and a bounded lifetime.
//!
//! `WebhookTrigger` bound `0.0.0.0:<port>` and accepted every POST on its path. There was no
//! signature check, no authentication hook, no principal, and no body policy — a malformed
//! body was wrapped as a JSON string and delivered as a trigger event indistinguishable from
//! a deliberate one. Any caller who could reach the port could start application-defined
//! agent work.
//!
//! The listener also outlived its consumer. `axum::serve` was spawned with no shutdown
//! signal, so dropping the event stream left the port bound: handlers logged that the
//! subscriber was gone, returned `200`, and discarded the request, while a restart on the
//! same port failed.

#![cfg(feature = "ambient")]

use adk_agent::ambient::{EventSource, WebhookRequest, WebhookTrigger, WebhookVerifier};
use futures::StreamExt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Accepts only requests carrying the expected token.
#[derive(Debug)]
struct TokenVerifier {
    token: String,
    calls: Arc<AtomicUsize>,
}

impl WebhookVerifier for TokenVerifier {
    fn verify(&self, request: &WebhookRequest<'_>) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match request.header("x-token") {
            Some(value) if value == self.token => Ok("trusted-caller".to_string()),
            Some(_) => Err("token mismatch".to_string()),
            None => Err("missing x-token".to_string()),
        }
    }
}

/// A free port, released before the trigger binds it.
async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Posts `body` with optional headers and returns the status code.
async fn post(port: u16, path: &str, body: &str, headers: &[(&str, &str)]) -> u16 {
    let mut request = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .body(body.to_string());
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.send().await.expect("request must reach the listener").status().as_u16()
}

// ── The trust boundary ────────────────────────────────────────────────

#[tokio::test]
async fn an_externally_reachable_webhook_without_a_verifier_refuses_to_start() {
    // The dangerous configuration must fail where the mistake is still cheap, rather than
    // silently exposing a remote trigger.
    let trigger = WebhookTrigger::new(0, "/hook")
        .with_bind_address(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));

    let message = match trigger.subscribe().await {
        Ok(_) => panic!("an unauthenticated externally reachable webhook must not start"),
        Err(error) => error.to_string(),
    };
    assert!(message.contains("verifier"), "the error must say what is missing: {message}");
}

#[tokio::test]
async fn the_default_bind_is_loopback_not_every_interface() {
    let trigger = WebhookTrigger::new(8080, "/hook");
    assert!(
        trigger.bind_address().ip().is_loopback(),
        "binding every interface must be an explicit choice, not the default"
    );
}

#[tokio::test]
async fn an_externally_reachable_webhook_with_a_verifier_starts() {
    let port = free_port().await;
    let trigger = WebhookTrigger::new(0, "/hook")
        .with_bind_address(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
        .with_verifier(Arc::new(TokenVerifier {
            token: "secret".to_string(),
            calls: Arc::new(AtomicUsize::new(0)),
        }));

    assert!(trigger.subscribe().await.is_ok(), "a verified trigger must start");
}

#[tokio::test]
async fn an_unauthorized_request_is_rejected_and_produces_no_event() {
    let port = free_port().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let trigger = WebhookTrigger::new(port, "/hook").with_verifier(Arc::new(TokenVerifier {
        token: "secret".to_string(),
        calls: Arc::clone(&calls),
    }));

    let mut stream = trigger.subscribe().await.expect("subscribe");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert_eq!(post(port, "/hook", r#"{"a":1}"#, &[]).await, 401, "no credential");
    assert_eq!(
        post(port, "/hook", r#"{"a":1}"#, &[("x-token", "wrong")]).await,
        401,
        "wrong credential"
    );

    // No event may be delivered for a rejected request.
    let delivered =
        tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await;
    assert!(delivered.is_err(), "a rejected request must not trigger agent work");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the verifier saw both attempts");
}

#[tokio::test]
async fn an_authorized_request_carries_its_principal() {
    let port = free_port().await;
    let trigger = WebhookTrigger::new(port, "/hook").with_verifier(Arc::new(TokenVerifier {
        token: "secret".to_string(),
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    let mut stream = trigger.subscribe().await.expect("subscribe");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let status = post(port, "/hook", r#"{"a":1}"#, &[("x-token", "secret")]).await;
    assert_eq!(status, 200);

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("an authorized request must produce an event")
        .expect("stream must yield");

    assert_eq!(
        event.principal.as_deref(),
        Some("trusted-caller"),
        "a handler must be able to tell an authorized trigger from an anonymous one"
    );
    assert_eq!(event.payload, serde_json::json!({ "a": 1 }));
}

// ── Body policy ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_malformed_body_is_rejected_rather_than_wrapped_as_a_string() {
    let port = free_port().await;
    let trigger = WebhookTrigger::new(port, "/hook");
    let mut stream = trigger.subscribe().await.expect("subscribe");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert_eq!(post(port, "/hook", "not json at all", &[]).await, 400);

    let delivered =
        tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await;
    assert!(delivered.is_err(), "unparseable input must not become a trigger event");
}

#[tokio::test]
async fn a_malformed_body_is_accepted_when_the_caller_opts_in() {
    let port = free_port().await;
    let trigger = WebhookTrigger::new(port, "/hook").accept_non_json();
    let mut stream = trigger.subscribe().await.expect("subscribe");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert_eq!(post(port, "/hook", "plain text", &[]).await, 200);

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .expect("the opt-in must still deliver")
        .expect("stream must yield");
    assert_eq!(event.payload, serde_json::json!("plain text"));
}

#[tokio::test]
async fn an_oversized_body_is_rejected() {
    let port = free_port().await;
    let trigger = WebhookTrigger::new(port, "/hook").with_max_body_bytes(16);
    let _stream = trigger.subscribe().await.expect("subscribe");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let big = format!(r#"{{"a":"{}"}}"#, "x".repeat(256));
    assert_eq!(post(port, "/hook", &big, &[]).await, 413);
}

// ── Lifecycle ─────────────────────────────────────────────────────────

#[tokio::test]
async fn dropping_the_stream_releases_the_port() {
    let port = free_port().await;

    {
        let trigger = WebhookTrigger::new(port, "/hook");
        let stream = trigger.subscribe().await.expect("subscribe");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(post(port, "/hook", r#"{"a":1}"#, &[]).await, 200, "listening");
        drop(stream);
    }

    // Rebinding is the observable consequence: before, the server kept the port after its
    // consumer went away, so a restart on the same port failed.
    let rebound = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let trigger = WebhookTrigger::new(port, "/hook");
            if let Ok(stream) = trigger.subscribe().await {
                return stream;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;

    assert!(rebound.is_ok(), "the port was still held after the consumer was dropped");
}
