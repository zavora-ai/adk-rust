use crate::{
    audio::{AudioChunk, AudioFormat},
    error::Result,
    transport::event::{TransportControl, TransportEvent},
};
use serde_json::Value;

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
        let msg: Value = serde_json::from_str(message).map_err(|e| {
            crate::error::RealtimeError::provider(format!("Invalid Twilio JSON: {}", e))
        })?;

        let event = msg.get("event").and_then(|v| v.as_str()).unwrap_or("");

        match event {
            "start" => {
                let call_id = msg
                    .get("start")
                    .and_then(|s| s.get("callSid"))
                    .and_then(|s| s.as_str())
                    .map(String::from);
                let stream_id = msg
                    .get("start")
                    .and_then(|s| s.get("streamSid"))
                    .and_then(|s| s.as_str())
                    .map(String::from);
                Ok(Some(TransportEvent::Started { call_id, stream_id, participant_id: None }))
            }
            "media" => {
                if let Some(payload) =
                    msg.get("media").and_then(|m| m.get("payload")).and_then(|p| p.as_str())
                {
                    use base64::Engine;
                    let data =
                        base64::engine::general_purpose::STANDARD.decode(payload).map_err(|e| {
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

                    let chunk =
                        AudioChunk::from_i16_samples(&samples_16khz, AudioFormat::pcm16_16khz());

                    Ok(Some(TransportEvent::Audio {
                        chunk,
                        timestamp_ms: None,
                        sequence: None,
                        source: None,
                    }))
                } else {
                    Ok(None)
                }
            }
            "dtmf" => {
                if let Some(digit) =
                    msg.get("dtmf").and_then(|d| d.get("digit")).and_then(|d| d.as_str())
                {
                    Ok(Some(TransportEvent::Dtmf { digit: digit.to_string(), source: None }))
                } else {
                    Ok(None)
                }
            }
            "stop" => Ok(Some(TransportEvent::Stopped { reason: None })),
            _ => Ok(None),
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

        serde_json::json!({
            "event": "media",
            "streamSid": stream_id,
            "media": {
                "payload": payload
            }
        })
        .to_string()
    }

    pub fn serialize_control(&self, stream_id: &str, control: &TransportControl) -> Option<String> {
        match control {
            TransportControl::ClearQueue => Some(
                serde_json::json!({
                    "event": "clear",
                    "streamSid": stream_id
                })
                .to_string(),
            ),
            TransportControl::Mark { name } => Some(
                serde_json::json!({
                    "event": "mark",
                    "streamSid": stream_id,
                    "mark": {
                        "name": name
                    }
                })
                .to_string(),
            ),
            TransportControl::Mute(_) => None, // Not directly supported by Twilio Media Streams in this way
        }
    }
}
