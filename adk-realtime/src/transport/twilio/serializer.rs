use crate::{
    audio::{AudioChunk, AudioFormat},
    error::Result,
    transport::event::{TransportControl, TransportEvent},
};
use serde_json::Value;

/// Serializer for converting between Twilio Media Streams JSON and TransportEvents.
pub struct TwilioMediaSerializer {
    format: AudioFormat,
}
impl Default for TwilioMediaSerializer {
    fn default() -> Self {
        Self::new()
    }
}

impl TwilioMediaSerializer {
    pub fn new() -> Self {
        Self { format: AudioFormat::g711_ulaw() }
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
                    let chunk =
                        AudioChunk::from_base64(payload, self.format.clone()).map_err(|e| {
                            crate::error::RealtimeError::provider(format!(
                                "Invalid base64 payload: {}",
                                e
                            ))
                        })?;
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
        let payload = audio.to_base64();
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
