//! Preflight estimates for token-billed backends (spec 3.7 §7.9).
//!
//! Before a priced call, the runtime opens a hold capped at the call's estimated cost; a hold
//! the run's budget cannot cover refuses the call before any image is released (V-I6). A
//! per-unit backend's estimate is its price ([`PriceRef::PerUnit`](crate::PriceRef)). A
//! token-billed backend declares a [`TokenEstimate`]: visual tokens from the sent
//! dimensions, prompt tokens, and output tokens up to the call's cap, at its rates. Its model
//! call then meters the tokens it used, so the vision hold commits units at zero cost.

use serde::{Deserialize, Serialize};

/// How a provider counts an image's input tokens.
///
/// # Example
///
/// ```
/// use adk_vision::ImageTokens;
///
/// // Claude: one token per 28-pixel patch.
/// assert_eq!(ImageTokens::Claude.tokens((1024, 768)), 37 * 28);
/// // Gemini: 258 tokens up to 384 px a side, then 258 per 768-pixel tile.
/// assert_eq!(ImageTokens::GeminiTiles.tokens((384, 300)), 258);
/// assert_eq!(ImageTokens::GeminiTiles.tokens((1600, 900)), 258 * 3 * 2);
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum ImageTokens {
    /// `⌈w/28⌉ × ⌈h/28⌉` visual tokens of the image the model processes.
    Claude,
    /// 258 tokens for an image whose sides are at most 384 px, otherwise 258 per 768×768 tile.
    GeminiTiles,
    /// A fixed number of tokens per image.
    Fixed {
        /// Tokens per image.
        tokens: u64,
    },
    /// `⌈w/side⌉ × ⌈h/side⌉` tiles of one token each: NVIDIA's Nemotron VL and Nano Omni,
    /// and Muse, with 32-pixel tiles.
    Tiles {
        /// The tile side in pixels.
        side: u32,
    },
}

impl ImageTokens {
    /// The input tokens of one image of `size` pixels.
    pub fn tokens(&self, (width, height): (u32, u32)) -> u64 {
        let (w, h) = (u64::from(width), u64::from(height));
        match self {
            Self::Claude => w.div_ceil(28) * h.div_ceil(28),
            Self::GeminiTiles if w <= 384 && h <= 384 => 258,
            Self::GeminiTiles => 258 * w.div_ceil(768) * h.div_ceil(768),
            Self::Fixed { tokens } => *tokens,
            Self::Tiles { side } => {
                let side = u64::from((*side).max(1));
                w.div_ceil(side) * h.div_ceil(side)
            }
        }
    }
}

/// A token-billed backend's preflight: how its images count, its rates, and the output cap
/// assumed when a call sets none.
///
/// # Example
///
/// ```
/// use adk_vision::{ImageTokens, TokenEstimate};
///
/// // $3 per million input tokens, $15 per million output tokens.
/// let sonnet = TokenEstimate::new(ImageTokens::Claude, 3_000_000, 15_000_000).with_max_output_tokens(1_024);
/// // One 560×280 crop (200 visual tokens), a 400-character prompt (100 tokens), 1,024 output tokens.
/// assert_eq!(sonnet.estimate_micro_usd(&[(560, 280)], 400, None), 900 + 15_360);
/// // A call that caps its own output at 100 tokens.
/// assert_eq!(sonnet.estimate_micro_usd(&[(560, 280)], 400, Some(100)), 900 + 1_500);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TokenEstimate {
    /// How the provider counts image tokens.
    pub image_tokens: ImageTokens,
    /// Input price in micro-USD per million tokens.
    pub input_micro_usd_per_million: u64,
    /// Output price in micro-USD per million tokens.
    pub output_micro_usd_per_million: u64,
    /// The output tokens assumed when a call sets no cap.
    pub max_output_tokens: u32,
}

impl TokenEstimate {
    /// Characters per prompt token assumed for the estimate.
    pub const CHARS_PER_TOKEN: u64 = 4;

    /// An estimate at these rates, assuming 4,096 output tokens when a call sets no cap.
    pub fn new(
        image_tokens: ImageTokens,
        input_micro_usd_per_million: u64,
        output_micro_usd_per_million: u64,
    ) -> Self {
        Self {
            image_tokens,
            input_micro_usd_per_million,
            output_micro_usd_per_million,
            max_output_tokens: 4_096,
        }
    }

    /// Sets the output tokens assumed when a call sets no cap.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = tokens;
        self
    }

    /// The input tokens of `images` (sent sizes) and a prompt of `prompt_chars` characters.
    pub fn input_tokens(&self, images: &[(u32, u32)], prompt_chars: usize) -> u64 {
        let visual: u64 = images.iter().map(|size| self.image_tokens.tokens(*size)).sum();
        visual + (prompt_chars as u64).div_ceil(Self::CHARS_PER_TOKEN)
    }

    /// The most the call can cost, in micro-USD, rounded up.
    pub fn estimate_micro_usd(
        &self,
        images: &[(u32, u32)],
        prompt_chars: usize,
        max_output_tokens: Option<u32>,
    ) -> u64 {
        let input = self.input_tokens(images, prompt_chars);
        let output = u64::from(max_output_tokens.unwrap_or(self.max_output_tokens));
        (input.saturating_mul(self.input_micro_usd_per_million)
            + output.saturating_mul(self.output_micro_usd_per_million))
        .div_ceil(1_000_000)
    }
}
