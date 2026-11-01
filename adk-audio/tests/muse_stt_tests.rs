//! `MuseStt` streams audio frames to the Muse realtime session and reports its events as
//! `TranscriptEvent`s: one `Final` per endpointed turn, and the session's billed audio.
#![cfg(feature = "muse")]

use adk_audio::{AudioFrame, MuseStt, SttOptions, SttProvider, TranscriptEvent};
use adk_model::muse::MuseConfig;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

#[tokio::test]
async fn a_live_session_yields_partials_one_final_per_turn_and_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut socket = tokio_tungstenite::accept_async(tcp).await.expect("upgrade");
        let Some(Ok(Message::Text(handshake))) = socket.next().await else { panic!("handshake") };
        let handshake: Value = serde_json::from_str(&handshake).unwrap();
        let send = |value: Value| Message::Text(value.to_string().into());
        socket.send(send(json!({ "sessionId": "s1" }))).await.unwrap();
        let mut audio = 0;
        while let Some(Ok(message)) = socket.next().await {
            match message {
                Message::Binary(bytes) => audio += bytes.len(),
                Message::Text(text) if text.contains("endStream") => break,
                _ => {}
            }
        }
        for event in [
            json!({ "type": "speechStart", "turnId": 1, "audioProcessedMs": 40 }),
            json!({ "type": "transcript", "transcript": "habari", "final": false }),
            json!({ "type": "transcript", "transcript": "habari yako", "final": true }),
            json!({ "type": "speechEnd", "turnId": 1 }),
            json!({ "type": "speechComplete", "turnId": 1, "transcript": "habari yako" }),
            json!({ "type": "audioProgress", "audioProcessedMs": 640 }),
        ] {
            socket.send(send(event)).await.unwrap();
        }
        socket
            .close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))
            .await
            .unwrap();
        (handshake, audio)
    });

    let stt = MuseStt::new(MuseConfig::new("test-key", "").with_base_url(base)).unwrap();
    let frames = futures::stream::iter([
        AudioFrame::new(vec![0_u8; 3_200], 16_000, 1),
        AudioFrame::new(vec![0_u8; 3_200], 16_000, 1),
    ]);
    let options = SttOptions::default().with_language("sw").with_keywords(["habari"]);
    let events: Vec<TranscriptEvent> = stt
        .transcribe_stream(Box::pin(frames), &options)
        .await
        .expect("session opens")
        .map(|event| event.expect("event"))
        .collect()
        .await;
    let (handshake, audio) = server.await.unwrap();

    assert_eq!(audio, 6_400, "every frame's audio is sent");
    assert_eq!(handshake["mode"], "ENDPOINTING");
    assert_eq!(handshake["languageBias"], json!(["sw"]));
    assert_eq!(handshake["keywords"], json!(["habari"]));
    let summary: Vec<String> = events
        .iter()
        .map(|event| match event {
            TranscriptEvent::Started { session_id } => format!("started:{session_id:?}"),
            TranscriptEvent::SpeechStarted { turn_id, .. } => format!("speech:{turn_id:?}"),
            TranscriptEvent::Partial { text, .. } => format!("partial:{text}"),
            TranscriptEvent::Final { turn_id, transcript } => {
                format!("final:{turn_id:?}:{}", transcript.text)
            }
            TranscriptEvent::SpeechEnded { turn_id, .. } => format!("ended:{turn_id:?}"),
            TranscriptEvent::Progress { processed_ms } => format!("progress:{processed_ms}"),
            TranscriptEvent::Usage { audio_ms } => format!("usage:{audio_ms}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        summary,
        [
            "started:Some(\"s1\")",
            "speech:Some(1)",
            "partial:habari",
            "partial:habari yako",
            "ended:Some(1)",
            "final:Some(1):habari yako",
            "progress:640",
            "usage:640",
        ]
    );
}

#[tokio::test]
async fn the_muse_clients_meter_themselves_so_the_provider_reports_no_billing() {
    let stt = MuseStt::new(MuseConfig::new("test-key", "")).unwrap();
    assert!(stt.billing().is_none());
}
