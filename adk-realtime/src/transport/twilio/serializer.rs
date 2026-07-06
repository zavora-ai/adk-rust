use crate::{
    audio::{AudioChunk, AudioFormat},
    error::Result,
    transport::event::{TransportControl, TransportEvent},
    transport::twilio::protocol::{
        ClearMessage, MarkData, MarkMessage, MediaData, MediaMessage, TwilioMessage,
    },
};

/// Serializer for converting between Twilio Media Streams JSON and TransportEvents.
pub struct TwilioMediaSerializer;

impl Default for TwilioMediaSerializer {
    fn default() -> Self {
        Self::new()
    }
}

impl TwilioMediaSerializer {
    pub fn new() -> Self {
        Self
    }

    /// Parse a Twilio WebSocket message into a TransportEvent.
    pub fn parse(&self, message: &str) -> Result<Option<TransportEvent>> {
        // First parse into a generic Value to check the event type
        let raw: serde_json::Value = serde_json::from_str(message).map_err(|e| {
            crate::error::RealtimeError::provider(format!("Invalid Twilio JSON: {}", e))
        })?;

        let event_type = raw.get("event").and_then(|v| v.as_str()).unwrap_or("unknown");

        // Match against supported event types before full deserialization
        match event_type {
            "connected" | "start" | "media" | "stop" | "mark" | "clear" | "dtmf" => {
                let msg: TwilioMessage = serde_json::from_value(raw).map_err(|e| {
                    crate::error::RealtimeError::provider(format!("Invalid Twilio Protocol: {}", e))
                })?;

                match msg {
                    TwilioMessage::Start(start) => Ok(Some(TransportEvent::Started {
                        call_id: Some(start.start.call_sid),
                        stream_id: Some(start.stream_sid),
                        participant_id: None,
                    })),
                    TwilioMessage::Media(media) => {
                        use base64::Engine;
                        let data = base64::engine::general_purpose::STANDARD
                            .decode(&media.media.payload)
                            .map_err(|e| {
                                crate::error::RealtimeError::provider(format!(
                                    "Invalid base64 payload: {}",
                                    e
                                ))
                            })?;

                        // Decode μ-law to PCM16 samples (8kHz)
                        let samples_8khz = crate::audio::g711::decode_ulaw_frame(&data);

                        // Upsample PCM16 from 8kHz to 16kHz for Gemini input by duplicating each sample
                        let mut samples_16khz = Vec::with_capacity(samples_8khz.len() * 2);
                        for &sample in &samples_8khz {
                            samples_16khz.push(sample);
                            samples_16khz.push(sample);
                        }

                        let chunk = AudioChunk::from_i16_samples(
                            &samples_16khz,
                            AudioFormat::pcm16_16khz(),
                        );

                        Ok(Some(TransportEvent::Audio {
                            chunk,
                            timestamp_ms: None,
                            sequence: None,
                            source: None,
                        }))
                    }
                    TwilioMessage::Dtmf(dtmf) => {
                        Ok(Some(TransportEvent::Dtmf { digit: dtmf.dtmf.digit, source: None }))
                    }
                    TwilioMessage::Stop(_) => Ok(Some(TransportEvent::Stopped { reason: None })),
                    _ => Ok(None),
                }
            }
            _ => {
                // Ignore unknown events (e.g. heartbeats)
                tracing::debug!("Ignoring unknown Twilio event: {}", event_type);
                Ok(None)
            }
        }
    }

    /// Serialize a TransportEvent or Control into a Twilio WebSocket message.
    pub fn serialize_audio(&self, stream_id: &str, audio: &AudioChunk) -> String {
        // Extract samples from the input PCM16 chunk (Gemini Live outputs 24kHz)
        let samples = audio.to_i16_samples().unwrap_or_default();

        // Downsample to 8kHz for Twilio
        let samples_8khz = match audio.format.sample_rate {
            24000 => {
                let mut downsampled = Vec::with_capacity(samples.len() / 3);
                for chunk in samples.chunks_exact(3) {
                    let avg = ((chunk[0] as i32 + chunk[1] as i32 + chunk[2] as i32) / 3) as i16;
                    downsampled.push(avg);
                }
                downsampled
            }
            16000 => {
                let mut downsampled = Vec::with_capacity(samples.len() / 2);
                for chunk in samples.chunks_exact(2) {
                    let avg = ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16;
                    downsampled.push(avg);
                }
                downsampled
            }
            _ => samples,
        };

        // Encode 8kHz PCM16 samples to μ-law bytes
        let ulaw_bytes = crate::audio::g711::encode_ulaw_frame(&samples_8khz);

        use base64::Engine;
        let payload = base64::engine::general_purpose::STANDARD.encode(&ulaw_bytes);

        let msg = TwilioMessage::Media(MediaMessage {
            sequence_number: None,
            stream_sid: stream_id.to_string(),
            media: MediaData { track: None, chunk: None, timestamp: None, payload },
        });

        serde_json::to_string(&msg).unwrap_or_default()
    }

    pub fn serialize_control(&self, stream_id: &str, control: &TransportControl) -> Option<String> {
        match control {
            TransportControl::ClearQueue => {
                let msg = TwilioMessage::Clear(ClearMessage { stream_sid: stream_id.to_string() });
                Some(serde_json::to_string(&msg).unwrap_or_default())
            }
            TransportControl::Mark { name } => {
                let msg = TwilioMessage::Mark(MarkMessage {
                    stream_sid: stream_id.to_string(),
                    mark: MarkData { name: name.to_string() },
                });
                Some(serde_json::to_string(&msg).unwrap_or_default())
            }
            TransportControl::Mute(_) => None, // Not directly supported by Twilio Media Streams in this way
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioFormat;

    #[test]
    fn test_parse_start() {
        let serializer = TwilioMediaSerializer::new();
        let json = r#"{
            "event": "start",
            "sequenceNumber": "1",
            "streamSid": "MZ123",
            "start": {
                "accountSid": "AC123",
                "streamSid": "MZ123",
                "callSid": "CA123",
                "tracks": ["inbound"],
                "mediaFormat": {
                    "encoding": "audio/x-mulaw",
                    "sampleRate": 8000,
                    "channels": 1
                }
            }
        }"#;

        let event = serializer.parse(json).unwrap().unwrap();
        match event {
            TransportEvent::Started { call_id, stream_id, .. } => {
                assert_eq!(call_id.unwrap(), "CA123");
                assert_eq!(stream_id.unwrap(), "MZ123");
            }
            _ => panic!("Expected Started event"),
        }
    }

    #[test]
    fn test_parse_media() {
        let serializer = TwilioMediaSerializer::new();
        // Base64 for a single 0x00 byte (which is -32124 in μ-law)
        let json = r#"{
            "event": "media",
            "streamSid": "MZ123",
            "media": {
                "payload": "AA=="
            }
        }"#;

        let event = serializer.parse(json).unwrap().unwrap();
        match event {
            TransportEvent::Audio { chunk, .. } => {
                assert_eq!(chunk.format.sample_rate, 16000);
                let samples = chunk.to_i16_samples().unwrap();
                // 1 byte of μ-law @ 8kHz -> 1 sample @ 8kHz -> 2 samples @ 16kHz
                assert_eq!(samples.len(), 2);
                assert_eq!(samples[0], -32124);
                assert_eq!(samples[1], -32124);
            }
            _ => panic!("Expected Audio event"),
        }
    }

    #[test]
    fn test_parse_stop() {
        let serializer = TwilioMediaSerializer::new();
        let json = r#"{
            "event": "stop",
            "streamSid": "MZ123",
            "stop": {
                "accountSid": "AC123",
                "callSid": "CA123"
            }
        }"#;

        let event = serializer.parse(json).unwrap().unwrap();
        match event {
            TransportEvent::Stopped { .. } => {}
            _ => panic!("Expected Stopped event"),
        }
    }

    #[test]
    fn test_parse_unknown_event() {
        let serializer = TwilioMediaSerializer::new();
        let json = r#"{
            "event": "heartbeat",
            "timestamp": "2023-01-01T00:00:00Z"
        }"#;

        let event = serializer.parse(json).unwrap();
        assert!(event.is_none());
    }

    #[test]
    fn test_serialize_audio() {
        let serializer = TwilioMediaSerializer::new();
        let samples = vec![0, 0, 0]; // 3 samples of 0
        let chunk = AudioChunk::from_i16_samples(&samples, AudioFormat::pcm16_24khz());

        let json = serializer.serialize_audio("MZ123", &chunk);
        let msg: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(msg["event"], "media");
        assert_eq!(msg["streamSid"], "MZ123");
        assert!(msg["media"]["payload"].is_string());
    }

    #[test]
    fn test_serialize_control_mark() {
        let serializer = TwilioMediaSerializer::new();
        let control = TransportControl::Mark { name: "test_mark".to_string() };

        let json = serializer.serialize_control("MZ123", &control).unwrap();
        let msg: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(msg["event"], "mark");
        assert_eq!(msg["streamSid"], "MZ123");
        assert_eq!(msg["mark"]["name"], "test_mark");
    }

    #[test]
    fn test_serialize_control_clear() {
        let serializer = TwilioMediaSerializer::new();
        let control = TransportControl::ClearQueue;

        let json = serializer.serialize_control("MZ123", &control).unwrap();
        let msg: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(msg["event"], "clear");
        assert_eq!(msg["streamSid"], "MZ123");
    }
}
