//! Interval capture for screenshot sources, and the image inside a tool's or an
//! observation's JSON.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use serde_json::Value;
use tokio::time::MissedTickBehavior;

use super::queue::{Pushed, spawn_frames};
use super::{FrameStream, SOURCE_QUEUE_FRAMES, Sampler};
use crate::error::{Result, VisionError};
use crate::media::{DecodeLimits, DecodedImage, Frame, FramePixels, FrameSourceId};
use crate::traits::Sampling;

/// One screenshot, as encoded bytes.
#[async_trait]
pub(crate) trait Capture: Send + Sync + 'static {
    async fn capture(&self) -> Result<Vec<u8>>;
}

/// Frames from `capture`, one per `interval` or per sampling interval, whichever is longer.
/// A capture that takes longer than the interval skips the ticks it missed; a capture error
/// ends the stream with that error.
pub(crate) fn interval_frames(
    id: FrameSourceId,
    capture: Arc<dyn Capture>,
    interval: Duration,
    sampling: Sampling,
) -> Result<FrameStream> {
    let sampler = Sampler::new(sampling)?;
    let period = interval.max(sampler.interval());
    Ok(spawn_frames(SOURCE_QUEUE_FRAMES, move |tx| async move {
        let started = tokio::time::Instant::now();
        let mut ticks = tokio::time::interval(period);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let limits = DecodeLimits::default();
        let mut index = 0u64;
        loop {
            // The scheduled tick, not the moment the capture returned, so a slow capture does
            // not squeeze the gap to the next frame.
            let tick = ticks.tick().await;
            let pts = tick.saturating_duration_since(started);
            let bytes = match capture.capture().await {
                Ok(bytes) => bytes,
                Err(err) => return tx.fail(err),
            };
            let captured_at = Utc::now();
            let fitter = sampler.clone();
            let decoded = tokio::task::spawn_blocking(move || {
                DecodedImage::decode(&bytes, &limits).map(|(pixels, _)| fitter.fit(pixels))
            })
            .await
            .unwrap_or_else(|err| {
                Err(VisionError::Decode(format!("screenshot decode panicked: {err}")))
            });
            let pixels = match decoded {
                Ok(pixels) => pixels,
                Err(err) => return tx.fail(err),
            };
            let frame = Frame {
                source: id.clone(),
                index,
                pts,
                captured_at,
                pixels: FramePixels::Ephemeral(Arc::new(pixels)),
            };
            index += 1;
            if tx.push(Ok(frame)) == Pushed::Closed {
                return;
            }
        }
    }))
}

/// The longest JSON path searched for an image.
const MAX_DEPTH: usize = 8;

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let payload = match text.strip_prefix("data:") {
        Some(url) => url.split_once(";base64,")?.1,
        None => text,
    };
    base64::engine::general_purpose::STANDARD.decode(payload.trim()).ok()
}

fn bytes_of(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::String(text) => decode_base64(text),
        Value::Array(items) => {
            items.iter().map(|item| item.as_u64().and_then(|b| u8::try_from(b).ok())).collect()
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}

fn mime_of(map: &serde_json::Map<String, Value>) -> Option<&str> {
    ["mime_type", "mimeType", "media_type", "mediaType"]
        .iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str))
}

fn find(value: &Value, depth: usize) -> Option<(Vec<u8>, String)> {
    if depth > MAX_DEPTH {
        return None;
    }
    match value {
        Value::Object(map) => {
            // An inline part or an MCP image block: bytes beside an image MIME type.
            if let Some(mime) = mime_of(map).filter(|m| m.starts_with("image/"))
                && let Some(bytes) = map.get("data").and_then(bytes_of)
            {
                return Some((bytes, mime.to_string()));
            }
            for key in ["base64_image", "screenshot", "image", "imageBase64", "image_base64", "png"]
            {
                if let Some(bytes) = map.get(key).and_then(Value::as_str).and_then(decode_base64) {
                    return Some((bytes, String::new()));
                }
            }
            for key in ["inline_data", "response", "output", "content", "result", "observation"] {
                if let Some(found) = map.get(key).and_then(|v| find(v, depth + 1)) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| find(item, depth + 1)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

/// The screenshot inside a tool result or a computer-use observation, as encoded bytes and
/// its MIME type (empty when the JSON does not name one).
///
/// Reads, in order: an inline part or MCP image block (`{"mime_type": "image/png", "data":
/// ...}`, the data base64 or a byte array, as `adk-tool`'s MCP envelope carries it under
/// `inline_data`); a base64 or data-URL field named `base64_image` (`browser_screenshot`),
/// `screenshot`, `image`, `imageBase64`, `image_base64` or `png`; then the same inside
/// `inline_data`, `response`, `output`, `content`, `result` and `observation`, at most
/// eight levels deep.
///
/// # Errors
///
/// [`VisionError::Parse`] when no image is found.
///
/// # Example
///
/// ```
/// use adk_vision::stream::observation_image;
/// use serde_json::json;
///
/// // A computer-use-mcp `snapshot` through adk-tool's MCP envelope.
/// let observation = json!({
///     "response": { "output": "1 window" },
///     "inline_data": [{ "mime_type": "image/png", "data": [137, 80, 78, 71] }],
///     "file_data": []
/// });
/// let (bytes, mime) = observation_image(&observation).unwrap();
/// assert_eq!((bytes.as_slice(), mime.as_str()), (&[137u8, 80, 78, 71][..], "image/png"));
/// ```
pub fn observation_image(observation: &Value) -> Result<(Vec<u8>, String)> {
    find(observation, 0).ok_or_else(|| VisionError::Parse {
        backend: "screen".into(),
        message: "the observation carries no image: expected an image part, an MCP image block, or a base64 `screenshot` field".into(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn mcp_blocks_data_urls_and_nested_fields_are_found() {
        let block =
            json!({ "content": [{ "type": "image", "mimeType": "image/jpeg", "data": "AQID" }] });
        assert_eq!(observation_image(&block).unwrap(), (vec![1, 2, 3], "image/jpeg".into()));
        let url = json!({ "output": { "screenshot": "data:image/png;base64,AQID" } });
        assert_eq!(observation_image(&url).unwrap().0, vec![1, 2, 3]);
        let browser = json!({ "success": true, "base64_image": "AQID" });
        assert_eq!(observation_image(&browser).unwrap().0, vec![1, 2, 3]);
    }

    #[test]
    fn json_without_an_image_is_refused() {
        let err = observation_image(&json!({ "output": "no windows" })).unwrap_err();
        assert!(matches!(err, VisionError::Parse { .. }), "{err}");
        // Text MIME types are not images.
        let text = json!({ "inline_data": [{ "mime_type": "text/plain", "data": "AQID" }] });
        assert!(observation_image(&text).is_err());
    }
}
