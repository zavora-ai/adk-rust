//! The Muse realtime transcription client against a scripted WebSocket server: the
//! ack-gated handshake with the key only in its frame, event mapping and turn attribution,
//! the `endStream` drain, close codes, the lead window, dropping the stream, and metering.
#![cfg(feature = "muse-realtime")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    ErrorCategory, InMemorySpendLedger, ModelCallScope, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod,
};
use adk_model::muse::{AsrEvent, MuseConfig, MuseRealtimeAsrClient, RealtimeAsrOptions};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

type Socket = WebSocketStream<TcpStream>;

/// What the server saw of the upgrade request: its URI and whether it had an
/// `Authorization` header.
type Upgrade = Arc<Mutex<Option<(String, bool)>>>;

/// A server that accepts one session and runs `script` on it; returns the base URL.
// tungstenite's handshake callback returns its own `ErrorResponse`, an HTTP response.
#[allow(clippy::result_large_err)]
async fn serve<F, Fut>(script: F) -> (String, Upgrade, tokio::task::JoinHandle<()>)
where
    F: FnOnce(Socket) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let upgrade: Upgrade = Arc::default();
    let seen = Arc::clone(&upgrade);
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let socket =
            tokio_tungstenite::accept_hdr_async(tcp, |request: &Request, response: Response| {
                *seen.lock().unwrap() = Some((
                    request.uri().to_string(),
                    request.headers().contains_key("authorization"),
                ));
                Ok(response)
            })
            .await
            .expect("upgrade");
        script(socket).await;
    });
    (format!("http://{addr}"), upgrade, task)
}

fn client(base: &str) -> MuseRealtimeAsrClient {
    MuseRealtimeAsrClient::new(MuseConfig::new("test-key", "").with_base_url(base)).unwrap()
}

/// Reads the handshake frame and acknowledges it.
async fn ack(socket: &mut Socket) -> Value {
    let Some(Ok(Message::Text(handshake))) = socket.next().await else {
        panic!("the first frame is the handshake");
    };
    send(socket, json!({ "sessionId": "session-1" })).await;
    serde_json::from_str(&handshake).expect("handshake JSON")
}

async fn send(socket: &mut Socket, event: Value) {
    socket.send(Message::Text(event.to_string().into())).await.expect("send");
}

/// Reads audio frames until `endStream`; returns the audio bytes received.
async fn until_end(socket: &mut Socket) -> usize {
    let mut bytes = 0;
    while let Some(Ok(message)) = socket.next().await {
        match message {
            Message::Binary(audio) => bytes += audio.len(),
            Message::Text(text) if text.contains("endStream") => return bytes,
            _ => {}
        }
    }
    panic!("the client ended without endStream");
}

async fn close(socket: &mut Socket, code: CloseCode, reason: &str) {
    socket
        .close(Some(CloseFrame { code, reason: reason.to_string().into() }))
        .await
        .expect("close");
}

fn chunks(count: usize, bytes: usize) -> impl futures::Stream<Item = Vec<u8>> + Send + 'static {
    futures::stream::iter((0..count).map(move |_| vec![0_u8; bytes]))
}

async fn collect(
    client: &MuseRealtimeAsrClient,
    count: usize,
) -> Vec<Result<AsrEvent, adk_core::AdkError>> {
    client.transcribe(chunks(count, 3_200)).await.expect("session opens").collect().await
}

#[tokio::test]
async fn the_key_travels_only_in_the_handshake_and_no_audio_precedes_the_ack() {
    let (base, upgrade, server) = serve(|mut socket| async move {
        let Some(Ok(Message::Text(handshake))) = socket.next().await else {
            panic!("the first frame is the handshake");
        };
        let handshake: Value = serde_json::from_str(&handshake).unwrap();
        assert_eq!(handshake["authorization"]["accessToken"], "Bearer test-key");
        assert_eq!(handshake["audioEncoding"], "PCM_16KHZ");
        assert_eq!(handshake["model"], "muse-voice-transcribe-1.0");
        assert_eq!(handshake["mode"], "ENDPOINTING");
        assert_eq!(handshake["partialMode"], "CUMULATIVE");
        assert_eq!(handshake["emitAudioProgress"], true);
        assert_eq!(handshake["keywords"], json!(["ADK-Rust"]));
        // Nothing else arrives before the acknowledgement.
        assert!(tokio::time::timeout(Duration::from_millis(150), socket.next()).await.is_err());
        send(&mut socket, json!({ "sessionId": "session-1" })).await;
        assert_eq!(until_end(&mut socket).await, 6_400);
        close(&mut socket, CloseCode::Normal, "").await;
    })
    .await;
    let client = client(&base).with_options(
        RealtimeAsrOptions::default().with_keywords(["ADK-Rust"]).with_session_id("s 1"),
    );
    let events = collect(&client, 2).await;
    server.await.unwrap();
    let (uri, authorization) = upgrade.lock().unwrap().clone().unwrap();
    assert_eq!(uri, "/asr/realtime?sessionId=s+1");
    assert!(!authorization, "no Authorization header");
    assert!(!uri.contains("test-key"));
    assert_eq!(
        events.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        [AsrEvent::Started { session_id: "session-1".into() }, AsrEvent::Usage { audio_ms: 0 }]
    );
}

#[tokio::test]
async fn events_follow_the_latest_turn_and_trailing_events_drain_before_close() {
    let (base, _, server) = serve(|mut socket| async move {
        ack(&mut socket).await;
        until_end(&mut socket).await;
        // Everything below arrives after endStream, before the close.
        send(&mut socket, json!({ "type": "speechStart", "turnId": 1, "audioProcessedMs": 120 }))
            .await;
        send(&mut socket, json!({ "type": "transcript", "transcript": "hel", "final": false }))
            .await;
        send(&mut socket, json!({ "type": "future", "detail": "ignored" })).await;
        socket.send(Message::Binary(vec![1, 2, 3].into())).await.unwrap();
        send(&mut socket, json!({ "type": "speechEnd", "turnId": 1 })).await;
        send(&mut socket, json!({ "type": "speechComplete", "turnId": 1, "transcript": "hello" }))
            .await;
        send(&mut socket, json!({ "type": "speaker", "label": "A" })).await;
        send(&mut socket, json!({ "type": "audioProgress", "audioProcessedMs": 2_000 })).await;
        close(&mut socket, CloseCode::Normal, "").await;
    })
    .await;
    let events: Vec<AsrEvent> =
        collect(&client(&base), 3).await.into_iter().map(Result::unwrap).collect();
    server.await.unwrap();
    assert_eq!(
        events,
        [
            AsrEvent::Started { session_id: "session-1".into() },
            AsrEvent::SpeechStart { turn_id: 1, audio_processed_ms: Some(120) },
            AsrEvent::Transcript {
                turn_id: Some(1),
                text: "hel".into(),
                is_final: false,
                audio_processed_ms: None,
            },
            AsrEvent::SpeechEnd { turn_id: 1 },
            AsrEvent::SpeechComplete { turn_id: 1, text: "hello".into() },
            AsrEvent::Speaker { label: "A".into() },
            AsrEvent::AudioProgress { audio_processed_ms: 2_000 },
            AsrEvent::Usage { audio_ms: 2_000 },
        ]
    );
}

#[tokio::test]
async fn close_codes_map_to_error_categories() {
    let cases = [
        (
            CloseCode::Policy,
            "backlog too large",
            ErrorCategory::InvalidInput,
            "model.muse.realtime_rejected",
        ),
        (
            CloseCode::Error,
            "Max session duration reached",
            ErrorCategory::Unavailable,
            "model.muse.session_limit",
        ),
        (CloseCode::Error, "internal", ErrorCategory::Unavailable, "model.muse.realtime_failed"),
        (
            CloseCode::Again,
            "slow down",
            ErrorCategory::RateLimited,
            "model.muse.realtime_rate_limited",
        ),
    ];
    for (code, reason, category, error_code) in cases {
        let (base, _, server) = serve(move |mut socket| async move {
            ack(&mut socket).await;
            close(&mut socket, code, reason).await;
        })
        .await;
        let events = collect(&client(&base), 1).await;
        server.await.unwrap();
        let error = events.into_iter().find_map(Result::err).expect("an error");
        assert_eq!((error.category, error.code), (category, error_code), "{reason}");
        assert_eq!(error.details.provider.as_deref(), Some("muse"));
    }
}

#[tokio::test]
async fn a_dropped_connection_without_a_close_frame_is_incomplete() {
    let (base, _, server) = serve(|mut socket| async move {
        ack(&mut socket).await;
        drop(socket);
    })
    .await;
    let events = collect(&client(&base), 1).await;
    server.await.unwrap();
    let error = events.into_iter().find_map(Result::err).expect("an error");
    assert_eq!(error.category, ErrorCategory::Unavailable);
}

#[tokio::test]
async fn a_handshake_refused_or_never_acknowledged_fails_the_call() {
    let (base, _, server) = serve(|mut socket| async move {
        let _ = socket.next().await;
        close(&mut socket, CloseCode::Policy, "invalid key").await;
    })
    .await;
    let error = client(&base).transcribe(chunks(1, 320)).await.err().expect("refused");
    server.await.unwrap();
    assert_eq!(error.code, "model.muse.realtime_rejected");

    let (base, _, server) = serve(|mut socket| async move {
        let _ = socket.next().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    })
    .await;
    let client = client(&base).with_options(
        RealtimeAsrOptions::default().with_handshake_timeout(Duration::from_millis(100)),
    );
    let error = client.transcribe(chunks(1, 320)).await.err().expect("timed out");
    assert_eq!(
        (error.category, error.code),
        (ErrorCategory::Timeout, "model.muse.realtime_handshake")
    );
    server.abort();
}

/// Audio frames received until none arrives for 200 ms.
async fn audio_frames_until_quiet(socket: &mut Socket) -> usize {
    let mut count = 0;
    while let Ok(Some(Ok(message))) =
        tokio::time::timeout(Duration::from_millis(200), socket.next()).await
    {
        if matches!(message, Message::Binary(_)) {
            count += 1;
        }
    }
    count
}

#[tokio::test]
async fn audio_waits_inside_the_lead_window() {
    let (counts_tx, counts_rx) = oneshot::channel();
    let (base, _, server) = serve(|mut socket| async move {
        ack(&mut socket).await;
        let before = audio_frames_until_quiet(&mut socket).await;
        send(&mut socket, json!({ "type": "audioProgress", "audioProcessedMs": 150 })).await;
        let after = before + audio_frames_until_quiet(&mut socket).await;
        let _ = counts_tx.send((before, after));
        close(&mut socket, CloseCode::Normal, "").await;
    })
    .await;
    // 50 ms chunks at 16 kHz under a 100 ms lead window.
    let client = client(&base)
        .with_options(RealtimeAsrOptions::default().with_lead_window(Duration::from_millis(100)));
    let mut events = client.transcribe(chunks(20, 1_600)).await.expect("session opens");
    let reader = tokio::spawn(async move { while events.next().await.is_some() {} });
    let (before, after) = counts_rx.await.expect("counts");
    server.await.unwrap();
    reader.await.unwrap();
    // 0, 50 and 100 ms sent fit 100 ms ahead of nothing processed; 150 ms processed lets
    // three more through.
    assert_eq!((before, after), (3, 6));
}

#[tokio::test]
async fn dropping_the_stream_releases_the_audio_and_closes_the_socket() {
    let (first_tx, first_rx) = oneshot::channel();
    let (base, _, server) = serve(|mut socket| async move {
        ack(&mut socket).await;
        let first = socket.next().await;
        let _ = first_tx.send(matches!(first, Some(Ok(Message::Binary(_)))));
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await;
    let (dropped_tx, dropped_rx) = oneshot::channel::<()>();
    let audio = async_stream::stream! {
        let _dropped_tx = dropped_tx;
        yield vec![0_u8; 320];
        std::future::pending::<()>().await;
    };
    let events = client(&base).transcribe(audio).await.expect("session opens");
    assert!(tokio::time::timeout(Duration::from_secs(5), first_rx).await.unwrap().unwrap());
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), dropped_rx)
        .await
        .expect("the audio is released")
        .expect_err("dropped, not completed");
    tokio::time::timeout(Duration::from_secs(5), server).await.expect("socket closed").unwrap();
}

async fn muse_spent(ledger: &InMemorySpendLedger) -> u64 {
    ledger.spent(&SpendKey::org("studio").with_vendor("muse"), SpendPeriod::Day).await.unwrap()
}

#[tokio::test]
async fn a_session_commits_the_audio_processed_and_a_cap_refuses_before_connecting() {
    let (base, _, server) = serve(|mut socket| async move {
        ack(&mut socket).await;
        until_end(&mut socket).await;
        send(&mut socket, json!({ "type": "transcript", "transcript": "hello", "final": true }))
            .await;
        send(&mut socket, json!({ "type": "audioProgress", "audioProcessedMs": 90_400 })).await;
        close(&mut socket, CloseCode::Normal, "").await;
    })
    .await;
    let ledger = Arc::new(InMemorySpendLedger::default());
    let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "studio");
    let asr = client(&base);
    let events = scope.clone().within(asr.transcribe(chunks(1, 320))).await.expect("opens");
    let _: Vec<_> = events.collect().await;
    server.await.unwrap();
    // 90.4 s bills as 90 s at $0.18 per hour.
    assert_eq!(muse_spent(&ledger).await, 4_500);

    // A full session is held up front: $0.18 does not fit under $0.10.
    let cap = SpendKey::org("studio").per(SpendPeriod::Day);
    let capped = Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(cap, 100_000)));
    let scope = ModelCallScope::new().with_spend_ledger(capped.clone(), "studio");
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", unused.local_addr().unwrap());
    let error =
        scope.within(client(&base).transcribe(chunks(1, 320))).await.err().expect("refused");
    assert_eq!(error.code, adk_core::SPEND_LIMIT_EXCEEDED_CODE);
    assert!(tokio::time::timeout(Duration::from_millis(100), unused.accept()).await.is_err());
}

#[tokio::test]
async fn a_session_without_a_transcript_is_not_billed() {
    let (base, _, server) = serve(|mut socket| async move {
        ack(&mut socket).await;
        until_end(&mut socket).await;
        send(&mut socket, json!({ "type": "audioProgress", "audioProcessedMs": 5_000 })).await;
        close(&mut socket, CloseCode::Again, "rate limited").await;
    })
    .await;
    let ledger = Arc::new(InMemorySpendLedger::default());
    let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "studio");
    let events = scope.within(client(&base).transcribe(chunks(1, 320))).await.expect("opens");
    let _: Vec<_> = events.collect().await;
    server.await.unwrap();
    assert_eq!(muse_spent(&ledger).await, 0);
}
