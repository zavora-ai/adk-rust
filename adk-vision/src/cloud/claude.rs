//! [`ClaudePixels`]: Claude answers in pixels of the image it processed.

use serde_json::{Value, json};

use super::adapter::{RegionAdapter, boxed, image_index, quad};
use crate::backend::InputLimits;
use crate::budget::ImageTokens;
use crate::geometry::CoordFrame;
use crate::traits::ImageRegion;

/// Claude's image tiers: how large an image it takes before resizing it itself.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeVisionTier {
    /// Claude Opus 4.7 and later, Sonnet 5 and later, Haiku 5.5, Fable and Mythos: 2,576 px
    /// on the long edge and 4,784 visual tokens of `⌈w/28⌉ × ⌈h/28⌉`.
    High,
    /// Earlier models: 1,568 px on the long edge and about 1,568 tokens of `w × h / 750`.
    Standard,
}

/// Claude's frame: pixels of the image it processed after its own resize.
///
/// Claude resizes an image above its tier's limit and then answers in the resized pixels.
/// The adapter's [`limits`](RegionAdapter::limits) keep every released image inside the
/// tier, so Claude never resizes and its answer is in pixels of the image as sent
/// ([`CoordFrame::SentPixels`]); the egress gate's resize is in the transform chain.
///
/// The high tier's limit is 2,576 px on the long edge and 3,605,000 pixels, the most for
/// which `⌈w/28⌉ × ⌈h/28⌉` stays within 4,784 tokens on any aspect ratio. Counted with
/// `count_tokens` on 2026-11-19: a 2356×1530 image costs 4,675 visual tokens (and 3 for the
/// image block) on `claude-sonnet-5-5` and `claude-haiku-5-5`, and a 2576×1673 one is
/// resized by the API.
///
/// # Example
///
/// ```
/// use adk_vision::cloud::{ClaudePixels, RegionAdapter};
///
/// let claude = ClaudePixels::for_model("claude-sonnet-5-5");
/// // 35 × 20 visual tokens.
/// assert_eq!(claude.image_tokens().tokens((960, 540)), 700);
/// // A 3024×1964 screenshot is downscaled until Claude would not resize it.
/// let (w, h) = claude.limits().fit((3024, 1964)).unwrap();
/// assert!(w.div_ceil(28) * h.div_ceil(28) <= 4784);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudePixels {
    tier: ClaudeVisionTier,
}

impl ClaudePixels {
    /// The adapter for a tier.
    pub fn new(tier: ClaudeVisionTier) -> Self {
        Self { tier }
    }

    /// The adapter for a Claude model ID, first-party, Bedrock or Vertex.
    pub fn for_model(model: &str) -> Self {
        Self::new(tier_for_model(model))
    }

    /// The tier.
    pub fn tier(&self) -> ClaudeVisionTier {
        self.tier
    }
}

fn tier_for_model(model: &str) -> ClaudeVisionTier {
    if crate::screen::claude_high_tier(model) {
        ClaudeVisionTier::High
    } else {
        ClaudeVisionTier::Standard
    }
}

impl RegionAdapter for ClaudePixels {
    fn name(&self) -> &str {
        "claude-pixels"
    }

    fn frame(&self) -> CoordFrame {
        CoordFrame::SentPixels
    }

    fn limits(&self) -> InputLimits {
        match self.tier {
            ClaudeVisionTier::High => InputLimits::long_edge(2576).with_max_pixels(3_605_000),
            ClaudeVisionTier::Standard => InputLimits::long_edge(1568).with_max_pixels(1_150_000),
        }
    }

    fn image_tokens(&self) -> ImageTokens {
        match self.tier {
            ClaudeVisionTier::High => ImageTokens::Claude,
            // `w × h / 750` within the tier's 1,150,000-pixel limit stays under 1,568.
            ClaudeVisionTier::Standard => ImageTokens::Fixed { tokens: 1_568 },
        }
    }

    fn region_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "label": {"type": "string", "description": "What the region is."},
                "box": {
                    "type": "array",
                    "items": {"type": "number"},
                    "description": "[x_min, y_min, x_max, y_max] in pixels of the image."
                }
            },
            "required": ["label", "box"],
            "additionalProperties": false
        })
    }

    fn instructions(&self, sizes: &[(u32, u32)]) -> String {
        let images = match sizes {
            [(w, h)] => format!("The image is {w}x{h} pixels."),
            many => many
                .iter()
                .enumerate()
                .map(|(i, (w, h))| format!("Image {i} is {w}x{h} pixels."))
                .collect::<Vec<_>>()
                .join(" "),
        };
        format!(
            "{images} Give each region's `box` as [x_min, y_min, x_max, y_max] in pixels of \
             that image, origin at the top-left corner, x to the right and y down, the box \
             tight around the region."
        )
    }

    fn parse_region(
        &self,
        item: &Value,
        sizes: &[(u32, u32)],
    ) -> Result<Option<ImageRegion>, String> {
        let image = image_index(item, sizes)?;
        let Some(raw) = quad(item, &["box", "bbox", "bbox_2d"])? else {
            return Ok(None);
        };
        Ok(Some(boxed(self.frame(), raw, item, image)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_follow_the_model() {
        for high in [
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-opus-5-5",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-fable-5-2",
            "anthropic.claude-sonnet-5-5",
            "claude-opus-4-7@20260101",
        ] {
            assert_eq!(tier_for_model(high), ClaudeVisionTier::High, "{high}");
        }
        for standard in ["claude-haiku-4-5", "claude-sonnet-4-6", "claude-opus-4-6", "claude-3-7"] {
            assert_eq!(tier_for_model(standard), ClaudeVisionTier::Standard, "{standard}");
        }
    }

    #[test]
    fn token_formula_matches_count_tokens() {
        let claude = ClaudePixels::new(ClaudeVisionTier::High);
        // Measured with count_tokens on claude-sonnet-5-5 and claude-haiku-5-5, less the
        // nine tokens of the text block and the three of the image block.
        for (size, measured) in [
            ((960, 540), 712),
            ((1000, 1000), 1308),
            ((1001, 1001), 1308),
            ((1568, 1018), 2084),
            ((2356, 1530), 4687),
        ] {
            assert_eq!(claude.image_tokens().tokens(size), measured - 12, "{size:?}");
        }
    }

    #[test]
    fn the_limit_keeps_every_aspect_within_the_token_cap() {
        let limits = ClaudePixels::new(ClaudeVisionTier::High).limits();
        for (w, h) in [(3024, 1964), (5000, 5000), (2576, 2576), (8000, 1000), (2000, 2000)] {
            let (fw, fh) = limits.fit((w, h)).unwrap_or((w, h));
            assert!(fw.max(fh) <= 2576);
            assert!(u64::from(fw.div_ceil(28)) * u64::from(fh.div_ceil(28)) <= 4784, "{w}x{h}");
        }
    }
}
