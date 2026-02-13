use crate::audio::AudioFormat;
use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ServerEvent};
use crate::model::RealtimeModel;
use crate::session::{BoxedSession, RealtimeSession};
use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};
use tracing::{error, info, warn};

use rand::Rng;
use rtc::data_channel::RTCDataChannelId;
/// OpenAI Realtime API WebRTC implementation.
///
/// This module provides a production-grade WebRTC implementation for OpenAI's Realtime API
/// using the `rtc` crate (Sans-IO). It handles signaling, media packetization,
/// and low-latency audio streaming with strict RFC compliance.
// IMPORTS FROM 'rtc' CRATE (webrtc-rs/rtc v0.8.5)
use rtc::peer_connection::RTCPeerConnection;
use rtc::peer_connection::configuration::{RTCConfigurationBuilder, media_engine::MediaEngine};
use rtc::peer_connection::event::{RTCDataChannelEvent, RTCPeerConnectionEvent};
use rtc::peer_connection::message::RTCMessage;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::peer_connection::state::{RTCIceGatheringState, RTCPeerConnectionState};
use rtc::peer_connection::transport::{CandidateConfig, CandidateHostConfig, RTCIceCandidate};
use rtc::rtp::header::Header;
use rtc::rtp::packet::Packet;
use rtc::rtp_transceiver::RTCRtpSenderId;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc::sansio::Protocol;
use rtc::shared::{TaggedBytesMut, TransportContext, TransportProtocol}; // For SSRC generation

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1/realtime";
// Input is 24kHz PCM16. 20ms = 480 samples.
// 480 samples * 2 bytes = 960 bytes per chunk.
const PCM_BYTES_PER_FRAME: usize = 960;

/// WebRTC Opus always uses a 48kHz clock (RFC 7587), regardless of input sample rate.
const RTP_CLOCK_RATE: u32 = 48000;
const FRAME_DURATION_MS: u32 = 20;

/// RTP timestamp increment for each 20ms frame.
/// (48,000Hz * 20ms) / 1000 = 960.
const RTP_TIMESTAMP_INCREMENT: u32 = (RTP_CLOCK_RATE * FRAME_DURATION_MS) / 1000;

pub struct OpenAiWebRtcModel {
    pub model_id: String,
    pub api_key: String,
}

impl OpenAiWebRtcModel {
    pub fn new(model_id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self { model_id: model_id.into(), api_key: api_key.into() }
    }
}

#[async_trait]
impl RealtimeModel for OpenAiWebRtcModel {
    fn provider(&self) -> &str {
        "openai"
    }
    fn model_id(&self) -> &str {
        &self.model_id
    }
    // Note: We accept pcm16_24khz input, but we will encode to Opus 48kHz for the wire
    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz()]
    }
    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz()]
    }
    fn available_voices(&self) -> Vec<&str> {
        vec!["alloy", "echo", "shimmer"]
    }

    async fn connect(&self, _config: RealtimeConfig) -> Result<BoxedSession> {
        let (tx, rx_cmd) = mpsc::channel(100);
        let (event_tx, rx_event) = mpsc::channel(100);
        let (close_tx, close_rx) = mpsc::channel(1);

        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| RealtimeError::connection(e.to_string()))?;
        let local_addr =
            socket.local_addr().map_err(|e| RealtimeError::connection(e.to_string()))?;

        let mut me = MediaEngine::default();
        me.register_default_codecs().map_err(|e| RealtimeError::connection(e.to_string()))?;
        let mut pc =
            RTCPeerConnection::new(RTCConfigurationBuilder::new().with_media_engine(me).build())
                .map_err(|e| RealtimeError::connection(e.to_string()))?;

        // 2. Add Audio Transceiver
        let _transceiver_id = pc
            .add_transceiver_from_kind(RtpCodecKind::Audio, None)
            .map_err(|e| RealtimeError::connection(e.to_string()))?;

        // Polling pc might help populate senders
        let _ = pc.poll_timeout();

        let sender_id = pc.get_senders().next();

        // 3. Create Data Channel "oai-events"
        // OpenAI requires this exact label.
        let dc = pc
            .create_data_channel("oai-events", None)
            .map_err(|e| RealtimeError::connection(e.to_string()))?;
        let dc_id = dc.id();

        // 4. Offer/Answer
        let offer = pc.create_offer(None).map_err(|e| RealtimeError::connection(e.to_string()))?;
        pc.set_local_description(offer.clone())
            .map_err(|e| RealtimeError::connection(e.to_string()))?;

        // 4.1 THE FIX: Manually add a local host candidate (rtc is sans-IO)
        let local_ip = local_addr.ip().to_string();
        let local_port = local_addr.port();
        info!("Adding local host candidate: {}:{}", local_ip, local_port);

        let candidate_config = CandidateHostConfig {
            base_config: CandidateConfig {
                network: "udp".to_owned(),
                address: local_ip,
                port: local_port,
                component: 1, // RTP
                ..Default::default()
            },
            ..Default::default()
        }
        .new_candidate_host()
        .map_err(|e| {
            RealtimeError::connection(format!("Failed to create host candidate: {}", e))
        })?;

        let local_candidate_init =
            RTCIceCandidate::from(&candidate_config).to_json().map_err(|e| {
                RealtimeError::connection(format!("Failed to convert candidate to JSON: {}", e))
            })?;

        pc.add_local_candidate(local_candidate_init).map_err(|e| {
            RealtimeError::connection(format!("Failed to add local candidate: {}", e))
        })?;

        // 4.5 Spin the loop until Gathering is Complete
        // Since rtc is sans-IO, we must drive the state machine to generate candidates.
        info!("Gathering ICE candidates...");
        let gathering_start = Instant::now();
        let mut gathering_complete = false;

        while !gathering_complete {
            if gathering_start.elapsed() > std::time::Duration::from_secs(3) {
                warn!("ICE Gathering timed out, sending incomplete SDP");
                break;
            }

            // Drive internal timers
            if let Some(timeout) = pc.poll_timeout() {
                let now = Instant::now();
                if timeout <= now {
                    pc.handle_timeout(now).map_err(|e| RealtimeError::connection(e.to_string()))?;
                }
            }

            // Poll events to catch gathering completion
            while let Some(event) = pc.poll_event() {
                match event {
                    RTCPeerConnectionEvent::OnIceGatheringStateChangeEvent(state) => {
                        info!("ICE Gathering State: {:?}", state);
                        if state == RTCIceGatheringState::Complete {
                            gathering_complete = true;
                        }
                    }
                    _ => {}
                }
            }

            if !gathering_complete {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }

        // Retrieve the UPDATED SDP with candidates
        let final_offer = pc.local_description().ok_or_else(|| {
            RealtimeError::connection("Failed to get local description after gathering")
        })?;

        info!("Final SDP with candidates: {}", final_offer.sdp);

        // 5. Signaling
        let answer_sdp =
            exchange_sdp(&self.model_id, &self.api_key, final_offer.sdp.clone()).await?;
        pc.set_remote_description(
            RTCSessionDescription::answer(answer_sdp)
                .map_err(|e| RealtimeError::connection(e.to_string()))?,
        )
        .map_err(|e| RealtimeError::connection(e.to_string()))?;

        // 6. Start the Sans-IO Loop
        tokio::spawn(run_pc_loop(
            pc, socket, local_addr, sender_id, dc_id, rx_cmd, event_tx, close_rx,
        ));

        Ok(Box::new(WebRtcSession { tx, receiver: Arc::new(Mutex::new(rx_event)), close_tx }))
    }
}

async fn run_pc_loop(
    mut pc: RTCPeerConnection,
    socket: UdpSocket,
    local_addr: std::net::SocketAddr,
    mut sender_id: Option<RTCRtpSenderId>,
    dc_id: RTCDataChannelId,
    mut rx_cmd: mpsc::Receiver<ClientEvent>,
    event_tx: mpsc::Sender<Result<ServerEvent>>,
    mut close_rx: mpsc::Receiver<()>,
) {
    let socket = Arc::new(socket);

    // 1. Pre-allocated Buffers (Zero-alloc hot path)
    let mut net_buf = vec![0u8; 2000];
    let mut pcm_buffer = BytesMut::with_capacity(4096);
    let mut encoding_buffer = vec![0u8; 1024]; // Reusable Opus buffer

    // 2. State Flags
    let mut dc_connected = false;
    let mut pending_dc_messages: Vec<String> = Vec::with_capacity(50);

    // 3. RTP State (Stable SSRC)
    // We pick ONE SSRC and stick to it. This prevents decoder resets on the server.
    // OpenAI uses SSRC latching, so it will accept whatever we send first.
    let stable_ssrc: u32 = rand::thread_rng().r#gen();
    let mut timestamp = 0u32;
    let mut sequence_number = 0u16;

    let encoder = audiopus::coder::Encoder::new(
        audiopus::SampleRate::Hz24000,
        audiopus::Channels::Mono,
        audiopus::Application::Voip,
    )
    .expect("Opus encoder init failed");

    info!("WebRTC Loop Started. Audio SSRC: {}", stable_ssrc);

    loop {
        // Calculate sleep time for the strict Sans-IO timer
        let timeout = pc.poll_timeout();
        let sleep_fut = async {
            if let Some(to) = timeout {
                let now = Instant::now();
                if to > now {
                    tokio::time::sleep(to - now).await;
                }
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            biased; // Poll network/timers first, then app commands

            // A. Timer Handling
            _ = sleep_fut => {
                let _ = pc.handle_timeout(Instant::now());
            }

            // B. Network Ingress (UDP -> PC)
            res = socket.recv_from(&mut net_buf) => {
                if let Ok((n, addr)) = res {
                    let msg = TaggedBytesMut {
                        now: Instant::now(),
                        transport: TransportContext {
                            local_addr,
                            peer_addr: addr,
                            transport_protocol: TransportProtocol::UDP,
                            ecn: None,
                        },
                        message: BytesMut::from(&net_buf[..n]),
                    };
                    let _ = pc.handle_read(msg);
                }
            }

            // C. Application Commands (App -> PC)
            cmd = rx_cmd.recv() => {
                match cmd {
                    Some(ClientEvent::AudioDelta { audio, .. }) => {
                        pcm_buffer.put(audio);

                        // Chunk 20ms frames (960 bytes @ 24kHz PCM16)
                        while pcm_buffer.len() >= PCM_BYTES_PER_FRAME {
                            let frame_bytes = pcm_buffer.split_to(PCM_BYTES_PER_FRAME);

                            // Convert bytes to i16 for Opus
                            let pcm_samples: Vec<i16> = frame_bytes.chunks_exact(2)
                                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                .collect();

                            // Encode directly into pre-allocated buffer
                            if let Ok(len) = encoder.encode(&pcm_samples, &mut encoding_buffer) {
                                // Lazy sender discovery
                                if sender_id.is_none() { sender_id = pc.get_senders().next(); }

                                if let Some(sid) = sender_id {
                                    if let Some(mut sender) = pc.rtp_sender(sid) {
                                        let packet = Packet {
                                            header: Header {
                                                version: 2,
                                                padding: false,
                                                extension: false,
                                                marker: false,
                                                payload_type: 111,
                                                sequence_number,
                                                timestamp,
                                                ssrc: stable_ssrc, // Use our stable SSRC
                                                ..Default::default()
                                            },
                                            payload: Bytes::copy_from_slice(&encoding_buffer[..len]),
                                        };

                                        if let Err(e) = sender.write_rtp(packet) {
                                            warn!("RTP Write Error: {:?}", e);
                                        }

                                        sequence_number = sequence_number.wrapping_add(1);
                                        timestamp = timestamp.wrapping_add(RTP_TIMESTAMP_INCREMENT);
                                    }
                                }
                            }
                        }
                    }
                    Some(ClientEvent::ConversationItemCreate { item }) => {
                        let event = ClientEvent::ConversationItemCreate { item };
                        if let Ok(json) = serde_json::to_string(&event) {
                            if dc_connected {
                                // Fast path: Send immediately
                                if let Some(mut dc) = pc.data_channel(dc_id) {
                                    if let Err(e) = dc.send_text(json) {
                                        error!("DC Send Error: {:?}", e);
                                    }
                                }
                            } else {
                                // Queueing path: Store until open
                                if pending_dc_messages.len() < 50 {
                                    info!("Data Channel pending, queuing item...");
                                    pending_dc_messages.push(json);
                                } else {
                                    error!("Outgoing message queue full! Dropping message.");
                                }
                            }
                        }
                    }

                    // -- Added missing handlers for other event types to ensure complete coverage --
                     Some(ClientEvent::InputAudioBufferCommit) => {
                         let msg = serde_json::to_string(&ClientEvent::InputAudioBufferCommit).unwrap_or_default();
                         if dc_connected {
                            if let Some(mut dc) = pc.data_channel(dc_id) { let _ = dc.send_text(msg); }
                         } else if pending_dc_messages.len() < 50 { pending_dc_messages.push(msg); }
                     }
                     Some(ClientEvent::InputAudioBufferClear) => {
                         pcm_buffer.clear();
                         let msg = serde_json::to_string(&ClientEvent::InputAudioBufferClear).unwrap_or_default();
                         if dc_connected {
                            if let Some(mut dc) = pc.data_channel(dc_id) { let _ = dc.send_text(msg); }
                         } else if pending_dc_messages.len() < 50 { pending_dc_messages.push(msg); }
                     }
                     Some(ClientEvent::ResponseCreate { config }) => {
                         if let Ok(msg) = serde_json::to_string(&ClientEvent::ResponseCreate { config }) {
                             if dc_connected {
                                if let Some(mut dc) = pc.data_channel(dc_id) { let _ = dc.send_text(msg); }
                             } else if pending_dc_messages.len() < 50 { pending_dc_messages.push(msg); }
                         }
                     }
                     Some(ClientEvent::ResponseCancel) => {
                         let msg = serde_json::to_string(&ClientEvent::ResponseCancel).unwrap_or_default();
                         if dc_connected {
                            if let Some(mut dc) = pc.data_channel(dc_id) { let _ = dc.send_text(msg); }
                         } else if pending_dc_messages.len() < 50 { pending_dc_messages.push(msg); }
                     }
                     Some(ClientEvent::SessionUpdate { session }) => {
                         if let Ok(msg) = serde_json::to_string(&ClientEvent::SessionUpdate { session }) {
                             if dc_connected {
                                if let Some(mut dc) = pc.data_channel(dc_id) { let _ = dc.send_text(msg); }
                             } else if pending_dc_messages.len() < 50 { pending_dc_messages.push(msg); }
                         }
                     }

                    None => break,
                }
            }
            _ = close_rx.recv() => break,
        }

        // D. Output Events (PC -> App)
        while let Some(message) = pc.poll_read() {
            match message {
                RTCMessage::DataChannelMessage(_, msg) => {
                    if let Ok(event) = serde_json::from_slice::<ServerEvent>(&msg.data) {
                        let _ = event_tx.send(Ok(event)).await;
                    }
                }
                _ => {}
            }
        }

        // E. Network Egress (PC -> UDP)
        while let Some(msg) = pc.poll_write() {
            if let Err(e) = socket.send_to(&msg.message, msg.transport.peer_addr).await {
                error!("UDP Send failure: {}", e);
            }
        }

        // F. Event Polling (State Changes)
        while let Some(evt) = pc.poll_event() {
            match evt {
                RTCPeerConnectionEvent::OnDataChannel(RTCDataChannelEvent::OnOpen(id)) => {
                    if id == dc_id {
                        info!(
                            "OpenAI Data Channel Established - Flushing {} items",
                            pending_dc_messages.len()
                        );
                        dc_connected = true;

                        // FLUSH QUEUE
                        if let Some(mut dc) = pc.data_channel(dc_id) {
                            for msg in pending_dc_messages.drain(..) {
                                if let Err(e) = dc.send_text(msg) {
                                    error!("Failed to flush queued message: {:?}", e);
                                }
                            }
                        }
                    }
                }
                RTCPeerConnectionEvent::OnConnectionStateChangeEvent(state) => {
                    info!("Peer Connection State: {:?}", state);
                    if state == RTCPeerConnectionState::Failed
                        || state == RTCPeerConnectionState::Closed
                    {
                        error!("Connection failed or closed. Exiting loop.");
                        return; // Exit the task
                    }
                }
                _ => {}
            }
        }
    }
    let _ = pc.close();
}

async fn exchange_sdp(model_id: &str, api_key: &str, offer_sdp: String) -> Result<String> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| RealtimeError::connection(e.to_string()))?;

    let url = format!("{}?model={}", OPENAI_BASE_URL, model_id);
    let res = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/sdp")
        .body(offer_sdp)
        .send()
        .await
        .map_err(|e| RealtimeError::connection(e.to_string()))?;

    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(RealtimeError::connection(format!("Signaling Error: {}", text)));
    }
    res.text().await.map_err(|e| RealtimeError::connection(e.to_string()))
}

pub struct WebRtcSession {
    tx: mpsc::Sender<ClientEvent>,
    receiver: Arc<Mutex<mpsc::Receiver<Result<ServerEvent>>>>,
    close_tx: mpsc::Sender<()>,
}

#[async_trait]
impl RealtimeSession for WebRtcSession {
    fn session_id(&self) -> &str {
        "webrtc-session"
    }
    fn is_connected(&self) -> bool {
        true
    }
    async fn send_audio(&self, audio: &crate::audio::AudioChunk) -> Result<()> {
        self.send_event(ClientEvent::AudioDelta {
            event_id: None,
            audio: Bytes::from(audio.data.clone()),
            format: audio.format.clone(),
        })
        .await
    }
    async fn send_audio_base64(&self, audio_b64: &str) -> Result<()> {
        use base64::Engine;
        let audio = base64::engine::general_purpose::STANDARD
            .decode(audio_b64)
            .map_err(|e| RealtimeError::connection(e.to_string()))?;
        self.send_audio(&crate::audio::AudioChunk {
            data: audio,
            format: AudioFormat::pcm16_24khz(),
        })
        .await
    }
    async fn send_text(&self, text: &str) -> Result<()> {
        self.send_event(ClientEvent::ConversationItemCreate {
            item: serde_json::json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": text }] }),
        }).await
    }
    async fn send_tool_response(&self, res: crate::events::ToolResponse) -> Result<()> {
        self.send_event(ClientEvent::ConversationItemCreate {
            item: serde_json::json!({ "type": "function_call_output", "call_id": res.call_id, "output": res.output }),
        }).await
    }
    async fn commit_audio(&self) -> Result<()> {
        self.send_event(ClientEvent::InputAudioBufferCommit).await
    }
    async fn clear_audio(&self) -> Result<()> {
        self.send_event(ClientEvent::InputAudioBufferClear).await
    }
    async fn create_response(&self) -> Result<()> {
        self.send_event(ClientEvent::ResponseCreate { config: None }).await
    }
    async fn interrupt(&self) -> Result<()> {
        self.send_event(ClientEvent::ResponseCancel).await?;
        self.send_event(ClientEvent::InputAudioBufferClear).await
    }
    async fn send_event(&self, event: ClientEvent) -> Result<()> {
        self.tx.send(event).await.map_err(|e| RealtimeError::connection(e.to_string()))
    }
    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        self.receiver.lock().await.recv().await
    }
    fn events(
        &self,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<ServerEvent>> + Send + '_>> {
        let receiver = self.receiver.clone();
        Box::pin(async_stream::try_stream! {
            loop {
                let mut rx = receiver.lock().await;
                match rx.recv().await {
                    Some(Ok(event)) => yield event,
                    Some(Err(e)) => yield Err(e)?,
                    None => break,
                }
            }
        })
    }
    async fn close(&self) -> Result<()> {
        let _ = self.close_tx.send(()).await;
        Ok(())
    }
}
