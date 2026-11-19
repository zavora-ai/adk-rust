//! [`RegionAdapter`]: how one provider is asked for regions and how its answer is read.

use std::fmt::Debug;
use std::sync::Arc;

use adk_core::GenerateContentConfig;
use serde_json::Value;

use super::claude::ClaudePixels;
use super::gemini::GeminiBox2d;
use super::normalized::Normalized1000;
use crate::backend::InputLimits;
use crate::budget::ImageTokens;
use crate::geometry::{BBox, CoordFrame, RawRegion};
use crate::traits::ImageRegion;

/// The version of the region instructions every adapter writes into its prompt. Answers
/// recorded under one version replay only against that version's parser.
pub const PROMPT_VERSION: &str = "adk-vision/regions/1";

/// How one provider is asked for regions, and how its answer is read in the frame it
/// answers in (spec 3.7 §7.3).
///
/// [`LlmDescriber`](super::LlmDescriber) picks an adapter by model ID
/// ([`adapter_for_model`]); a model with no adapter answers in text and values only, and a
/// request for regions is refused with `Unsupported` rather than a guessed frame.
///
/// # Example
///
/// ```
/// use adk_vision::cloud::{RegionAdapter, adapter_for_model};
/// use adk_vision::{AxisOrder, CoordFrame};
///
/// let gemini = adapter_for_model("gemini-3.8-flash").unwrap();
/// assert_eq!(gemini.frame(), CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx });
/// let claude = adapter_for_model("claude-sonnet-5-5").unwrap();
/// assert_eq!(claude.frame(), CoordFrame::SentPixels);
/// assert!(adapter_for_model("my-fine-tune").is_none());
/// ```
pub trait RegionAdapter: Send + Sync + Debug {
    /// A stable name, such as `gemini-box2d`.
    fn name(&self) -> &str;

    /// The frame the provider's coordinates are in.
    fn frame(&self) -> CoordFrame;

    /// The largest image the provider takes without resizing it itself; the egress gate
    /// downscales to it and records the resize.
    fn limits(&self) -> InputLimits {
        InputLimits::unlimited()
    }

    /// How the provider counts an image's input tokens, for preflight estimates.
    fn image_tokens(&self) -> ImageTokens;

    /// The JSON Schema of one region in the answer.
    fn region_schema(&self) -> Value;

    /// The prompt text that fixes the region format, for images of `sizes` as sent.
    fn instructions(&self, sizes: &[(u32, u32)]) -> String;

    /// Reads one region of the answer, in [`frame`](Self::frame). `None` skips an item that
    /// names no region.
    ///
    /// # Errors
    ///
    /// A message naming what is malformed; the describer refuses the answer rather than
    /// guess.
    fn parse_region(
        &self,
        item: &Value,
        sizes: &[(u32, u32)],
    ) -> Result<Option<ImageRegion>, String>;

    /// Provider request options the adapter needs, such as reasoning off.
    fn configure(&self, config: &mut GenerateContentConfig) {
        let _ = config;
    }

    /// `true` when the provider takes a response schema; otherwise the answer's shape is
    /// fixed by the prompt alone and parsed defensively.
    fn structured_output(&self) -> bool {
        true
    }

    /// The spend-key vendor of the provider, when the model ID does not name it.
    fn vendor(&self) -> Option<&str> {
        None
    }
}

/// The adapter for a model ID, or `None` when no adapter is known for it.
///
/// | Model ID | Adapter | Frame |
/// |----------|---------|-------|
/// | `gemini-*` | [`GeminiBox2d`] | `box_2d` on 0–1000, y first |
/// | `claude-*` (Bedrock and Vertex IDs too) | [`ClaudePixels`] | pixels of the sent image |
/// | `nvidia/nemotron-*` vision-language models | [`Normalized1000::nemotron`] | 0–1000, x first, fixed by the prompt |
/// | `google/gemma-4-*` | [`Normalized1000::gemma4`] | 0–1000, x first, fixed by the prompt |
/// | `muse-*` | [`Normalized1000::muse`] | 0–1000, x first, fixed by the prompt |
pub fn adapter_for_model(model: &str) -> Option<Arc<dyn RegionAdapter>> {
    let id = model.to_ascii_lowercase();
    let name = id.rsplit('/').next().unwrap_or(&id);
    if name.contains("gemini") {
        return Some(Arc::new(GeminiBox2d::for_model(model)));
    }
    if name.contains("claude") {
        return Some(Arc::new(ClaudePixels::for_model(model)));
    }
    if name.starts_with("nemotron") && (name.contains("-vl") || name.contains("omni")) {
        return Some(Arc::new(Normalized1000::nemotron(model)));
    }
    if name.starts_with("gemma-4") {
        return Some(Arc::new(Normalized1000::gemma4(model)));
    }
    if name.starts_with("muse-") {
        return Some(Arc::new(Normalized1000::muse(model)));
    }
    None
}

/// The index of the image an item names: its `image` field, or 0 for a one-image request.
pub(crate) fn image_index(item: &Value, sizes: &[(u32, u32)]) -> Result<usize, String> {
    let index = match item.get("image") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(|| format!("`image` is not an image index: {v}"))?,
    };
    if index >= sizes.len() {
        return Err(format!("`image` {index} names no image; the request had {}", sizes.len()));
    }
    Ok(index)
}

/// The item's label, from `label`, `name` or `text`.
pub(crate) fn label(item: &Value) -> Option<String> {
    ["label", "name", "text"].iter().find_map(|k| item.get(*k)?.as_str().map(str::to_string))
}

/// Four finite numbers from the first of `keys` the item has.
pub(crate) fn quad(item: &Value, keys: &[&str]) -> Result<Option<[f32; 4]>, String> {
    let Some((key, value)) = keys.iter().find_map(|k| item.get(*k).map(|v| (*k, v))) else {
        return Ok(None);
    };
    let numbers = value
        .as_array()
        .filter(|a| a.len() == 4)
        .ok_or_else(|| format!("`{key}` is not four numbers: {value}"))?;
    let mut out = [0f32; 4];
    for (slot, n) in out.iter_mut().zip(numbers) {
        let n = n.as_f64().ok_or_else(|| format!("`{key}` holds a non-number: {value}"))?;
        if !n.is_finite() {
            return Err(format!("`{key}` holds a non-finite number: {value}"));
        }
        *slot = n as f32;
    }
    Ok(Some(out))
}

/// A box region with the item's label, in the frame the raw numbers are in.
pub(crate) fn boxed(frame: CoordFrame, raw: [f32; 4], item: &Value, image: usize) -> ImageRegion {
    let bbox: BBox = frame.box_from_raw(raw);
    let mut region = RawRegion::shape(crate::geometry::Shape::Box(bbox));
    region.label = label(item);
    region.score = item.get("score").and_then(Value::as_f64).map(|s| s as f32);
    ImageRegion { image, region }
}

/// The first JSON value in a model's text: the whole text, a fenced block, or the first
/// object or array in it.
pub(crate) fn extract_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Some(value);
    }
    if let Some(start) = trimmed.find("```") {
        let rest = &trimmed[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.find("```")
            && let Ok(value) = serde_json::from_str(rest[..end].trim())
        {
            return Some(value);
        }
    }
    let start = trimmed.find(['{', '['])?;
    serde_json::Deserializer::from_str(&trimmed[start..]).into_iter::<Value>().next()?.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_in_fences_and_prose() {
        assert_eq!(extract_json("{\"a\": 1}"), Some(serde_json::json!({"a": 1})));
        assert_eq!(
            extract_json("Here:\n```json\n{\"a\": [1, 2]}\n```\nDone."),
            Some(serde_json::json!({"a": [1, 2]}))
        );
        assert_eq!(
            extract_json("The boxes are [{\"b\": 2}] as asked."),
            Some(serde_json::json!([{"b": 2}]))
        );
        assert_eq!(extract_json("no json here"), None);
    }

    #[test]
    fn quads_refuse_what_is_not_four_numbers() {
        let ok = serde_json::json!({"bbox": [1, 2, 3, 4.5]});
        assert_eq!(quad(&ok, &["box_2d", "bbox"]).unwrap(), Some([1.0, 2.0, 3.0, 4.5]));
        assert_eq!(quad(&ok, &["box"]).unwrap(), None);
        assert!(quad(&serde_json::json!({"bbox": [1, 2, 3]}), &["bbox"]).is_err());
        assert!(quad(&serde_json::json!({"bbox": [1, "a", 3, 4]}), &["bbox"]).is_err());
    }

    #[test]
    fn image_indexes_are_checked() {
        let sizes = [(10, 10), (20, 20)];
        assert_eq!(image_index(&serde_json::json!({}), &sizes).unwrap(), 0);
        assert_eq!(image_index(&serde_json::json!({"image": 1}), &sizes).unwrap(), 1);
        assert!(image_index(&serde_json::json!({"image": 2}), &sizes).is_err());
    }

    #[test]
    fn adapters_are_chosen_by_model_id() {
        let name = |m: &str| adapter_for_model(m).map(|a| a.name().to_string());
        assert_eq!(name("gemini-3.8-flash").as_deref(), Some("gemini-box2d"));
        assert_eq!(name("models/gemini-4-hydrogen").as_deref(), Some("gemini-box2d"));
        assert_eq!(name("claude-haiku-5-5").as_deref(), Some("claude-pixels"));
        assert_eq!(name("us.anthropic.claude-sonnet-5-5").as_deref(), Some("claude-pixels"));
        assert_eq!(
            name("nvidia/nemotron-3.5-super-vl-120b-a12b").as_deref(),
            Some("normalized-1000")
        );
        assert_eq!(name("google/gemma-4-31b-it").as_deref(), Some("normalized-1000"));
        assert_eq!(name("muse-spark-1.3").as_deref(), Some("normalized-1000"));
        assert_eq!(name("nvidia/nemotron-3.5-embed-1b"), None);
        assert_eq!(name("gpt-5.2"), None);
    }
}
