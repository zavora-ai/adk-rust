//! Meta Muse provider implementation for ADK.
//!
//! This module provides support for Meta's Muse Spark models through the Meta
//! Model API (`https://api.meta.ai/v1`), which is drop-in compatible with the
//! OpenAI Chat Completions format:
//! - `muse-spark-1.3` ([`MuseConfig::spark_1_3`], the default, recommended for new work)
//! - `muse-spark-1.2` ([`MuseConfig::spark_1_2`], the previous version)
//! - `muse-spark-1.1` ([`MuseConfig::spark_1_1`], the original version)
//!
//! Any other model the API serves works through [`MuseConfig::new`], including
//! the `-contributor` checkpoints. Contributor pricing is discounted in exchange
//! for permission to train on request content, so no preset selects one.
//!
//! # Features
//!
//! - **Agentic Workflows**: Spark 1.3 is tuned for tool calling and multi-step agents
//! - **Tool Calling**: Full function/tool calling support
//! - **Streaming**: Real-time streaming responses
//! - **Long Context**: 1,048,576-token context window across the Spark line
//!
//! # Responses mode
//!
//! [`MuseConfig::with_api`]`(`[`MuseApi::Responses`]`)` drives `/responses` through the shared
//! Open Responses engine ([`OpenAIResponsesClient`](crate::openai::OpenAIResponsesClient)),
//! streamed or not, with the same `Llm` surface; responses are attributed and priced as
//! `muse`. It needs the `muse-responses` feature. Request options travel in
//! `GenerateContentConfig::extensions["muse"]`: `previous_response_id`, `store`, `reasoning`
//! (`effort`, `summary`), `include` and `background`. Combinations the API rejects —
//! `background` with streaming, `top_logprobs`, reasoning effort `none`,
//! `reasoning.encrypted_content` with `previous_response_id`, stop sequences, penalties —
//! fail with `model.unsupported_config` before any request is sent.
//!
//! # Beyond chat
//!
//! [`MuseResponsesClient`] drives the stateful Responses API (reasoning carried
//! across turns, stored responses, background runs). [`MuseImagesClient`],
//! [`MuseFilesClient`], and [`MuseAsrClient`] cover `muse-image-1.0` generation
//! and edits, the Files API, and `muse-voice-transcribe-1.0` transcription.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_model::muse::{MuseClient, MuseConfig};
//!
//! // Muse Spark 1.3, the recommended default
//! let client = MuseClient::new(MuseConfig::spark_1_3(
//!     std::env::var("META_API_KEY").unwrap()
//! ))?;
//!
//! // Muse Spark 1.2, the previous version
//! let previous = MuseClient::new(MuseConfig::spark_1_2(
//!     std::env::var("META_API_KEY").unwrap()
//! ))?;
//! ```

mod asr;
mod client;
mod config;
mod convert;
mod files;
mod images;
mod responses;

pub use asr::{MUSE_VOICE_MODEL, MuseAsrClient, TranscribeOptions, Transcript, TranscriptSegment};
pub use client::MuseClient;
pub use config::{MUSE_API_BASE, MuseApi, MuseConfig};
pub use files::{FileObject, MuseFilesClient};
pub use images::{ImageData, ImageOptions, ImagesResponse, MUSE_IMAGE_MODEL, MuseImagesClient};
pub use responses::{
    FunctionCallItem, MuseResponsesClient, ResponseItem, ResponseObject, ResponseRequest,
};
