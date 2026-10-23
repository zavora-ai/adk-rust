use super::*;
use async_openai::types::responses::{CreateResponseArgs, InputParam};
use serde_json::json;
#[test]
fn preserves_endpoint_paths() {
    assert_eq!(
        derive_ws_url("https://chatgpt.com/backend-api/codex"),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(derive_ws_url("http://localhost:8080/v1/"), "ws://localhost:8080/v1/responses");
}

#[test]
fn rejects_frames_that_are_not_objects() {
    for body in [json!([]), json!("request"), Value::Null] {
        let error = create_event(&body).unwrap_err();
        assert_eq!(error.code, "model.openai_responses.ws_serialize");
        assert_eq!(error.category, ErrorCategory::InvalidInput);
    }
}

#[test]
fn rejects_background_requests() {
    let error = create_event(&json!({"input":[],"background":true})).unwrap_err();
    assert_eq!(error.code, "model.openai_responses.ws_serialize");
    assert_eq!(error.category, ErrorCategory::InvalidInput);
}

#[test]
fn merges_required_beta_without_losing_caller_values() {
    for values in [
        vec!["caller=v1"],
        vec!["caller=v1", "other=v2"],
        vec!["caller=v1, responses_websockets=2026-02-06"],
    ] {
        let mut headers = HeaderMap::new();
        for value in &values {
            headers.append("openai-beta", HeaderValue::from_str(value).unwrap());
        }
        let mut secret = HeaderValue::from_static("sensitive=v1");
        secret.set_sensitive(true);
        headers.append("openai-beta", secret);
        let merged = upgrade_headers(headers).unwrap();
        let tokens: Vec<_> = merged
            .get_all("openai-beta")
            .iter()
            .flat_map(|value| value.to_str().unwrap().split(',').map(str::trim))
            .collect();
        assert_eq!(tokens.iter().filter(|token| **token == WS_BETA).count(), 1);
        for value in values {
            for token in value.split(',').map(str::trim) {
                assert!(tokens.contains(&token));
            }
        }
        assert!(tokens.contains(&"sensitive=v1"));
        assert!(merged.get_all("openai-beta").iter().any(HeaderValue::is_sensitive));
    }
}

#[tokio::test]
async fn retains_send_and_read_failure_identity() {
    use tokio::{io::AsyncWriteExt, net::TcpListener};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        socket.next().await.unwrap().unwrap();
        // Reserved data opcode: a real protocol read failure, rather than a provider error.
        socket.get_mut().write_all(&[0x83, 0x00]).await.unwrap();
        let _ = socket.next().await;
    });
    let transport = WsTransport::lazy(&endpoint, "key", WsOptions::default());
    transport.preconnect(HeaderMap::new()).await.unwrap();
    {
        let mut lease = transport.lease(transport.connection.clone().lock_owned().await);
        send(&mut lease, &json!({"type":"response.create"})).await.unwrap();
        let failure = read(&mut lease, Duration::from_secs(5)).await.err().unwrap();
        assert_eq!(failure.error.code, "model.openai_responses.ws_read");
        assert_eq!(failure.error.details.provider.as_deref(), Some("openai-responses"));
        let _ = lease.guard.socket.as_mut().unwrap().close(None).await;
        let error = send(&mut lease, &json!({"type":"response.create"})).await.unwrap_err();
        assert_eq!(error.code, "model.openai_responses.ws_send");
        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert_eq!(error.details.provider.as_deref(), Some("openai-responses"));
    }
    server.await.unwrap();
}
#[test]
fn extends_only_the_completed_baseline() {
    let request =
        json!({"model":"fixture", "store":false,"input":[{"role":"user","content":"first"}]});
    let output = json!({"id":"msg_1","status":"completed","role":"assistant","content":"answer"});
    let previous =
        Continuation { request: request.clone(), response: "resp_1".into(), output: vec![output] };
    let mut next = request;
    next["input"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","content":"answer"}),
        json!({"role":"user","content":"next"}),
    ]);
    let incremental = incremental_event(&next, &previous).unwrap();
    assert_eq!(incremental["previous_response_id"], "resp_1");
    assert_eq!(incremental["input"], json!([{"role":"user","content":"next"}]));
    next["model"] = json!("changed");
    assert!(incremental_event(&next, &previous).is_none());
    next["model"] = json!("fixture");
    next["input"][0]["content"] = json!("changed");
    assert!(incremental_event(&next, &previous).is_none());
}
#[test]
fn strips_transport_fields() {
    let event = create_event(
        &json!({"stream":true,"background":false,"previous_response_id":"caller","input":[]}),
    )
    .unwrap();
    assert_eq!(event, json!({"type":"response.create","previous_response_id":"caller","input":[]}));
}

#[test]
fn certificate_errors_are_definitive() {
    let invalid = || rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
    for error in [
        tungstenite::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, invalid())),
        tungstenite::Error::Tls(tungstenite::error::TlsError::Rustls(Box::new(invalid()))),
        tungstenite::Error::Tls(tungstenite::error::TlsError::InvalidDnsName),
    ] {
        let failure = classify(error);
        assert_eq!(failure.error.code, "model.openai_responses.ws_tls");
        assert!(!failure.error.is_retryable());
        assert_eq!(failure.refusal, WsRefusal::Definitive);
        assert_eq!(refusal(&failure.error), WsRefusal::Definitive);
    }
    for error in [
        tungstenite::Error::Io(std::io::ErrorKind::ConnectionReset.into()),
        tungstenite::Error::Tls(tungstenite::error::TlsError::Rustls(Box::new(
            rustls::Error::General("temporary failure".into()),
        ))),
    ] {
        let failure = classify(error);
        assert_eq!(failure.refusal, WsRefusal::Transient);
        assert_eq!(refusal(&failure.error), WsRefusal::Transient);
    }
}

#[test]
fn initialization_failure_does_not_refuse_websocket() {
    let error = AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.openai_responses.ws_tls_config",
        "Responses WebSocket TLS configuration failed",
    );
    assert!(error.is_retryable());
    assert_eq!(refusal(&error), WsRefusal::Transient);
}

#[tokio::test]
async fn transports_reject_untrusted_certificates() {
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use tokio::net::TcpListener;

    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.cert.der().clone()],
        PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("https://localhost:{}/v1", listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(acceptor.accept(socket).await.is_err());
        }
    });
    let transport = WsTransport::lazy(&endpoint, "key", WsOptions::default())
        .with_retry_config(RetryConfig::default());
    for _ in 0..2 {
        // A caller can retry the same transport after an unsuccessful TLS handshake.
        let error = transport.preconnect(HeaderMap::new()).await.unwrap_err();
        assert_eq!(error.code, "model.openai_responses.ws_tls");
        assert!(!error.is_retryable());
        assert!(!transport.is_connected());
    }
    assert_eq!(
        transport.connection.lock().await.preconnect_failure.as_ref().unwrap().1,
        WsRefusal::Definitive
    );
    let error = reqwest_openai::Client::new().get(&endpoint).send().await.unwrap_err();
    assert!(error.is_connect());
    tokio::time::timeout(Duration::from_secs(5), server).await.unwrap().unwrap();
}

#[test]
fn creates_events_without_http_fields() {
    let mut request = CreateResponseArgs::default()
        .model("gpt-5.6-terra")
        .input(InputParam::Text("hi".into()))
        .build()
        .unwrap();
    request.stream = Some(true);
    request.background = Some(false);
    let event = create_event(&serde_json::to_value(request).unwrap()).unwrap();
    assert_eq!(
        (
            &event["type"],
            &event["model"],
            &event["input"],
            event.get("stream"),
            event.get("background")
        ),
        (
            &Value::from("response.create"),
            &Value::from("gpt-5.6-terra"),
            &Value::from("hi"),
            None,
            None
        )
    );
}

#[test]
fn recognizes_terminal_outcomes() {
    for (kind, status, terminal) in [
        ("response.completed", Some("completed"), true),
        ("response.incomplete", Some("incomplete"), true),
        ("response.failed", None, true),
        ("response.cancelled", None, true),
        ("response.expired", Some("cancelled"), true),
        ("response.in_progress", Some("in_progress"), false),
        ("response.output_text.delta", None, false),
    ] {
        let mut event = serde_json::json!({"type": kind});
        if let Some(status) = status {
            event["response"] = serde_json::json!({"status": status});
        }
        assert_eq!(is_terminal(&event), terminal, "{event}");
    }
}

#[test]
fn classifies_errors_without_provider_text() {
    let cases = [
        (
            serde_json::json!({"type": "error", "status": 429,
            "error": {"code": "rate_limit_exceeded", "message": "slow down: secret"}}),
            ErrorCategory::RateLimited,
            Some("rate_limit_exceeded"),
        ),
        (
            serde_json::json!({"type": "error", "status": 400,
            "error": {"code": "previous_response_not_found", "message": "resp_secret"}}),
            ErrorCategory::NotFound,
            Some("previous_response_not_found"),
        ),
        (
            serde_json::json!({"type": "error",
            "error": {"code": "websocket_connection_limit_reached"}}),
            ErrorCategory::Unavailable,
            Some("websocket_connection_limit_reached"),
        ),
        (
            serde_json::json!({"type": "error", "status": 401,
            "error": {"code": "user content echoed", "message": "secret"}}),
            ErrorCategory::Unauthorized,
            None,
        ),
        (
            serde_json::json!({"type": "error", "code": "server_error", "message": "secret"}),
            ErrorCategory::Unavailable,
            Some("server_error"),
        ),
    ];
    for (event, category, code) in cases {
        let error = error_event(&event);
        assert_eq!(
            (
                error.category,
                error.code,
                error.details.metadata.get("provider_error_code").and_then(Value::as_str)
            ),
            (category, "model.openai_responses.ws_error", code),
            "{event}"
        );
        assert!(!error.to_problem_json().to_string().contains("secret"));
        assert_eq!(error.details.provider.as_deref(), Some("openai-responses"));
    }
}

#[test]
fn classifies_handshake_rejections() {
    let rejection = |status: u16| {
        let response = tungstenite::http::Response::builder().status(status).body(None).unwrap();
        classify(tungstenite::Error::Http(Box::new(response))).error
    };
    assert_eq!(rejection(401).category, ErrorCategory::Unauthorized);
    assert!(!rejection(401).is_retryable());
    assert!(!rejection(400).is_retryable());
    assert!(rejection(429).is_retryable());
    assert!(rejection(503).is_retryable());
    assert_eq!(rejection(503).details.upstream_status_code, Some(503));
    let invalid = "not a url".into_client_request().expect_err("invalid URL");
    assert_eq!(classify(invalid).error.category, ErrorCategory::InvalidInput);
}

#[test]
fn maps_url_schemes_and_paths() {
    for (base, expected) in [
        ("https://api.openai.com/v1", "wss://api.openai.com/v1/responses"),
        ("http://127.0.0.1:8080/v1/", "ws://127.0.0.1:8080/v1/responses"),
        ("https://proxy.example.com", "wss://proxy.example.com/v1/responses"),
        ("proxy.example.com", "wss://proxy.example.com/v1/responses"),
        ("proxy.example.com/v1", "wss://proxy.example.com/v1/responses"),
        ("wss://proxy.example.com/responses", "wss://proxy.example.com/responses"),
        ("https://proxy.example.com/v1/responses/", "wss://proxy.example.com/v1/responses"),
        (
            "https://proxy.example.com/custom?tenant=1",
            "wss://proxy.example.com/custom/responses?tenant=1",
        ),
    ] {
        assert_eq!(derive_ws_url(base), expected);
    }
}
#[test]
fn retains_input_error_identity() {
    let invalid_url = "not a url".into_client_request().unwrap_err();
    let error = classify(invalid_url).error;
    assert_eq!(error.category, ErrorCategory::InvalidInput);
    assert_eq!(error.code, "model.openai_responses.ws_invalid_url");
    assert_eq!(error.details.provider.as_deref(), Some("openai-responses"));
    let transport =
        WsTransport::lazy("https://fixture.test", "private-key\n", WsOptions::default());
    let error = transport.authorization(HeaderMap::new()).unwrap_err();
    assert_eq!(error.category, ErrorCategory::InvalidInput);
    assert_eq!(error.code, "model.openai_responses.ws_invalid_api_key");
    assert_eq!(error.details.provider.as_deref(), Some("openai-responses"));
    assert!(!error.to_problem_json().to_string().contains("private-key"));
    for error in [invalid(), timeout(), closed()] {
        assert_eq!(error.details.provider.as_deref(), Some("openai-responses"));
    }
}

#[test]
fn honors_proxy_bypass_rules() {
    use hyper_util::client::proxy::matcher::Matcher;
    let matcher = Matcher::builder()
        .http("http://proxy.invalid:8080")
        .https("http://proxy.invalid:8080")
        .no(".example.test,127.0.0.0/8,::1")
        .build();
    for url in [
        "wss://api.example.test/v1/responses",
        "ws://127.0.0.1:8080/v1/responses",
        "ws://[::1]/responses",
    ] {
        assert!(!uses_proxy(url, &matcher), "{url}");
    }
    for url in ["wss://notexample.test/responses", "ws://192.0.2.1/responses"] {
        assert!(uses_proxy(url, &matcher), "{url}");
    }
}

#[test]
fn tls_respects_the_installed_provider() {
    const CHILD: &str = "ADK_WS_TLS_PROVIDER_FIXTURE";
    if let Ok(mode) = std::env::var(CHILD) {
        if mode == "caller" {
            let mut provider = rustls::crypto::aws_lc_rs::default_provider();
            provider.cipher_suites.reverse();
            provider.install_default().expect("isolated caller provider");
        } else {
            adk_core::ensure_crypto_provider();
        }
        let expected = rustls::crypto::CryptoProvider::get_default().unwrap().clone();
        let config = tls_config().expect("platform verifier configuration");
        assert!(Arc::ptr_eq(config.crypto_provider(), &expected));
        return;
    }
    for mode in ["workspace", "caller"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "openai::ws_transport::tests::tls_respects_the_installed_provider"])
            .env(CHILD, mode)
            .status()
            .unwrap();
        assert!(status.success(), "{mode} provider must be preserved");
    }
}
