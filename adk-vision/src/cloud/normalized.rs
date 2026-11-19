//! [`Normalized1000`]: boxes fixed by the prompt to `[x_min, y_min, x_max, y_max]` on 0–1000.

use adk_core::GenerateContentConfig;
use serde_json::{Value, json};

use super::adapter::{RegionAdapter, boxed, image_index, quad};
use crate::budget::ImageTokens;
use crate::geometry::{AxisOrder, CoordFrame};
use crate::traits::ImageRegion;

/// A vision-language model whose box format the prompt fixes: `[x_min, y_min, x_max,
/// y_max]` on 0–1000, origin top-left, independent of the aspect ratio
/// ([`CoordFrame::Normalized`] with [`AxisOrder::Xy`]).
///
/// Models left to choose answer in different frames for different aspect ratios, so the
/// frame is stated in the prompt (versioned as
/// [`PROMPT_VERSION`](super::PROMPT_VERSION)) and answers are parsed defensively: the box
/// may be under `bbox`, `bbox_2d`, `box_2d` or `box`.
///
/// | Constructor | Models | Image tokens | Request options |
/// |-------------|--------|--------------|-----------------|
/// | [`nemotron`](Self::nemotron) | `nvidia/nemotron-3.5-super-vl-120b-a12b`, `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | `⌈w/32⌉ × ⌈h/32⌉` | `chat_template_kwargs.enable_thinking: false` |
/// | [`gemma4`](Self::gemma4) | `google/gemma-4-31b-it` | 260 per image | — |
/// | [`muse`](Self::muse) | `muse-*` | `⌈w/32⌉ × ⌈h/32⌉` | — |
///
/// The NVIDIA token counts were measured from `usage.prompt_tokens` on build.nvidia.com:
/// 960×540 costs 510 tokens and 1600×1600 costs 2,500 on Super VL and Nano Omni; Gemma 4
/// spends about 260 on any size.
///
/// # Example
///
/// ```
/// use adk_vision::cloud::{Normalized1000, RegionAdapter};
///
/// let super_vl = Normalized1000::nemotron("nvidia/nemotron-3.5-super-vl-120b-a12b");
/// assert_eq!(super_vl.image_tokens().tokens((960, 540)), 510);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalized1000 {
    tokens: ImageTokens,
    thinking_off: bool,
    vendor: Option<&'static str>,
}

const FRAME: CoordFrame = CoordFrame::Normalized { scale: 1000, order: AxisOrder::Xy };

impl Normalized1000 {
    /// An adapter counting image tokens with `tokens`.
    pub fn new(tokens: ImageTokens) -> Self {
        Self { tokens, thinking_off: false, vendor: None }
    }

    /// NVIDIA's Nemotron vision-language models. Their reasoning counts toward
    /// `max_tokens` and can leave the answer empty, so perception calls turn it off.
    pub fn nemotron(_model: &str) -> Self {
        Self { tokens: ImageTokens::Tiles { side: 32 }, thinking_off: true, vendor: Some("nvidia") }
    }

    /// Google's Gemma 4 as NVIDIA hosts it.
    pub fn gemma4(_model: &str) -> Self {
        Self {
            tokens: ImageTokens::Fixed { tokens: 260 },
            thinking_off: false,
            vendor: Some("nvidia"),
        }
    }

    /// Meta's Muse models through the Meta Model API.
    pub fn muse(_model: &str) -> Self {
        Self { tokens: ImageTokens::Tiles { side: 32 }, thinking_off: false, vendor: None }
    }
}

impl RegionAdapter for Normalized1000 {
    fn name(&self) -> &str {
        "normalized-1000"
    }

    fn frame(&self) -> CoordFrame {
        FRAME
    }

    fn image_tokens(&self) -> ImageTokens {
        self.tokens
    }

    fn region_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "label": {"type": "string"},
                "bbox": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "[x_min, y_min, x_max, y_max] normalized to 0-1000."
                }
            },
            "required": ["label", "bbox"],
            "additionalProperties": false
        })
    }

    fn instructions(&self, sizes: &[(u32, u32)]) -> String {
        let mut text = String::from(
            "Give each region's `bbox` as [x_min, y_min, x_max, y_max] normalised to 0-1000 \
             (0,0 is the top-left corner, 1000,1000 the bottom-right corner, independent of \
             the image's aspect ratio).",
        );
        if sizes.len() > 1 {
            text.push_str(" Name the image each region is in by its index, from 0.");
        }
        text
    }

    fn parse_region(
        &self,
        item: &Value,
        sizes: &[(u32, u32)],
    ) -> Result<Option<ImageRegion>, String> {
        let image = image_index(item, sizes)?;
        let Some(raw) = quad(item, &["bbox", "bbox_2d", "box_2d", "box"])? else {
            return Ok(None);
        };
        Ok(Some(boxed(FRAME, raw, item, image)))
    }

    fn configure(&self, config: &mut GenerateContentConfig) {
        if self.thinking_off {
            let openai = config.extensions.entry("openai").or_insert_with(|| json!({}));
            if let Some(fields) = openai.as_object_mut() {
                fields.insert("chat_template_kwargs".into(), json!({"enable_thinking": false}));
            }
        }
    }

    fn vendor(&self) -> Option<&str> {
        self.vendor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{BBox, Shape};

    #[test]
    fn nvidia_token_counts_match_the_recorded_usage() {
        let super_vl = Normalized1000::nemotron("nvidia/nemotron-3.5-super-vl-120b-a12b");
        for (size, tokens) in [
            ((960, 540), 510),
            ((512, 512), 256),
            ((800, 800), 625),
            ((900, 900), 841),
            ((1000, 1300), 1312),
            ((1600, 1600), 2500),
        ] {
            assert_eq!(super_vl.image_tokens().tokens(size), tokens, "{size:?}");
        }
        assert_eq!(
            Normalized1000::gemma4("google/gemma-4-31b-it").image_tokens().tokens((960, 540)),
            260
        );
    }

    #[test]
    fn every_documented_key_is_read_x_first() {
        let adapter = Normalized1000::nemotron("nvidia/nemotron-3.5-super-vl-120b-a12b");
        for key in ["bbox", "bbox_2d", "box_2d", "box"] {
            let item = json!({"label": "red", key: [78, 108, 335, 400]});
            let found = adapter.parse_region(&item, &[(960, 540)]).unwrap().unwrap();
            assert_eq!(found.region.shape, Shape::Box(BBox::new(78.0, 108.0, 335.0, 400.0)));
        }
    }

    #[test]
    fn nemotron_turns_reasoning_off() {
        let mut config = GenerateContentConfig::default();
        Normalized1000::nemotron("nvidia/nemotron-3.5-super-vl-120b-a12b").configure(&mut config);
        assert_eq!(
            config.extensions["openai"]["chat_template_kwargs"],
            json!({"enable_thinking": false})
        );
        let mut config = GenerateContentConfig::default();
        Normalized1000::gemma4("google/gemma-4-31b-it").configure(&mut config);
        assert!(config.extensions.is_empty());
    }
}
