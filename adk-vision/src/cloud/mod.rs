//! Cloud describers (spec 3.7 §7.7), feature `cloud`.
//!
//! [`LlmDescriber`] makes every `adk_core::Llm` that takes images a
//! [`Describer`](crate::Describer) without new client code: the released images go as
//! inline parts, the caller's schema as the response schema, and a [`RegionAdapter`]
//! picked by model ID fixes how regions are asked for and read.
//!
//! | Provider | Adapter | Frame | Image tokens (preflight) | Limit the gate downscales to |
//! |----------|---------|-------|--------------------------|------------------------------|
//! | Gemini | [`GeminiBox2d`] | `box_2d` `[ymin, xmin, ymax, xmax]` on 0–1000 | 258 up to 384 px a side, else 258 per 768 px tile | none |
//! | Claude 4.7+, Sonnet 5+, Haiku 5.5 | [`ClaudePixels`] | pixels of the image as sent | `⌈w/28⌉ × ⌈h/28⌉` | 2,576 px long edge, 3,605,000 pixels |
//! | Earlier Claude | [`ClaudePixels`] | pixels of the image as sent | up to 1,568 | 1,568 px long edge, 1,150,000 pixels |
//! | NVIDIA Nemotron VL, Nano Omni (`nim`) | [`Normalized1000`] | `[x_min, y_min, x_max, y_max]` on 0–1000, fixed by the prompt | `⌈w/32⌉ × ⌈h/32⌉` | none |
//! | Gemma 4 on NVIDIA (`nim`) | [`Normalized1000`] | the same | about 260 | none |
//! | Muse | [`Normalized1000`] | the same | `⌈w/32⌉ × ⌈h/32⌉` | none |
//! | Qwen3-VL on this machine (`mistralrs`, or a loopback server) | [`Normalized1000`] | the same, the model's native frame | `⌈w/32⌉ × ⌈h/32⌉` | none |
//!
//! Claude answers in pixels of the image it processed after its own resize; the Claude
//! adapter's limits keep every image inside Claude's tier, so Claude never resizes and its
//! answer is in pixels of the image as sent, with the gate's resize on the transform chain.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_model::{AnthropicClient, anthropic::AnthropicConfig};
//! use adk_vision::{DescribeSpec, LlmDescriber, VisionRuntime};
//!
//! let claude = AnthropicClient::new(AnthropicConfig::new(key, "claude-sonnet-5-5"))?;
//! let describer = LlmDescriber::remote(Arc::new(claude))?;
//! let runtime = VisionRuntime::builder()
//!     .remote_describer(Arc::new(describer))
//!     .egress_gate(gate)
//!     .build()?;
//! let answer = runtime
//!     .describe(&cx, &[&screenshot], &DescribeSpec::new("Where is Save?").with_regions())
//!     .await?;
//! // Every region is in source pixels of the screenshot.
//! ```

mod adapter;
mod claude;
mod describer;
mod gemini;
#[cfg(feature = "nim")]
pub mod nim;
mod normalized;
mod preflight;

pub(crate) use adapter::extract_json;
pub use adapter::{PROMPT_VERSION, RegionAdapter, adapter_for_model};
pub use claude::{ClaudePixels, ClaudeVisionTier};
pub use describer::LlmDescriber;
pub use gemini::{GeminiBox2d, GeminiMaskForm};
pub use normalized::Normalized1000;
pub use preflight::Preflight;
pub(crate) use preflight::Preflighted;
