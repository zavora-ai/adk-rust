//! Realtime connects install the process-level rustls crypto provider themselves.
//!
//! The `gemini` feature compiles in both the `ring` and `aws-lc-rs` providers, so rustls
//! cannot choose one on its own and panics when the first TLS connection is built. Under
//! nextest every test runs in its own process, so each starts with no provider installed.

#![cfg(any(feature = "gemini", feature = "openai"))]

use adk_realtime::RealtimeConfig;
use rustls::crypto::CryptoProvider;

#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_connect_installs_the_crypto_provider() {
    use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeSession};

    // A key that cannot form a URL fails the connect before any network access.
    let result = GeminiRealtimeSession::connect(
        GeminiLiveBackend::studio("not a key"),
        "models/gemini-3.1-flash-live-preview",
        RealtimeConfig::default(),
    )
    .await;
    assert!(result.is_err());
    assert!(CryptoProvider::get_default().is_some());
}

/// Builds a TLS client against a local listener, the step that panicked without a provider.
#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_connect_builds_tls_without_a_preinstalled_provider() {
    use adk_realtime::openai::OpenAIRealtimeSession;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            drop(socket);
        }
    });
    let result = OpenAIRealtimeSession::connect(
        &format!("wss://{address}/v1/realtime?model=gpt-realtime-2.1"),
        "sk-test",
        RealtimeConfig::default(),
    )
    .await;
    assert!(result.is_err());
    assert!(CryptoProvider::get_default().is_some());
}
