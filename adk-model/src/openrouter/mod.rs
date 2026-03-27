//! OpenRouter provider implementation for ADK.
//!
//! This module exposes OpenRouter as a first-class provider rather than routing
//! it through the generic OpenAI-compatible surface. It provides:
//!
//! - native chat-completions request and response types
//! - native Responses API request and response types
//! - discovery endpoint types for models, providers, credits, and endpoints
//! - shared OpenRouter configuration with app-attribution headers
//! - a reusable SSE frame decoder for native streaming support

mod adapter;
mod chat;
mod client;
mod config;
mod convert_chat;
mod convert_responses;
mod discovery;
mod error;
mod metadata;
mod responses;
mod stream;

pub use adapter::OpenRouterRequestOptions;
pub use chat::{
    OpenRouterChatChoice, OpenRouterChatContentPart, OpenRouterChatMessage,
    OpenRouterChatMessageContent, OpenRouterChatRequest, OpenRouterChatResponse,
    OpenRouterChatToolCall, OpenRouterChatUsage, OpenRouterFunctionDescription,
    OpenRouterImageConfig, OpenRouterPlugin, OpenRouterProviderMaxPrice,
    OpenRouterProviderPreferences, OpenRouterReasoningConfig, OpenRouterReasoningReplay,
    OpenRouterResponseFormat, OpenRouterTool, OpenRouterToolChoice,
};
pub use client::OpenRouterClient;
pub use config::{OPENROUTER_API_BASE, OpenRouterApiMode, OpenRouterConfig};
pub use convert_chat::{
    adk_contents_to_chat_messages, apply_reasoning_replay_to_chat_request,
    augment_chat_plugins_for_contents, chat_message_reasoning_to_parts,
    reasoning_replay_from_extension_value, reasoning_replay_to_extension_value,
};
pub use convert_responses::{
    adk_contents_to_response_input, apply_reasoning_replay_to_responses_request,
    responses_reasoning_items_to_parts,
};
pub use discovery::{
    OpenRouterBigNumber, OpenRouterCredits, OpenRouterCreditsEnvelope, OpenRouterDefaultParameters,
    OpenRouterEndpointStatus, OpenRouterModel, OpenRouterModelArchitecture,
    OpenRouterModelEndpoint, OpenRouterModelEndpoints, OpenRouterModelEndpointsEnvelope,
    OpenRouterModelPricing, OpenRouterModelsEnvelope, OpenRouterPerRequestLimits,
    OpenRouterPercentileStats, OpenRouterProvider, OpenRouterProvidersEnvelope,
    OpenRouterTopProviderInfo,
};
pub use error::{OpenRouterErrorBody, OpenRouterErrorEnvelope};
pub use metadata::{
    chat_response_citation_metadata, chat_response_provider_metadata, chat_usage_to_metadata,
    responses_citation_metadata, responses_provider_metadata, responses_usage_to_metadata,
};
pub use responses::{
    OpenRouterResponse, OpenRouterResponseInput, OpenRouterResponseInputContent,
    OpenRouterResponseInputContentPart, OpenRouterResponseInputItem, OpenRouterResponseOutputItem,
    OpenRouterResponseTextConfig, OpenRouterResponseTool, OpenRouterResponsesRequest,
    OpenRouterResponsesUsage,
};
pub use stream::{
    OpenRouterChatStream, OpenRouterChatStreamItem, OpenRouterResponsesStream,
    OpenRouterResponsesStreamEvent, OpenRouterResponsesStreamItem, OpenRouterStreamError,
    parse_chat_stream_frame, parse_responses_stream_frame,
};
pub use stream::{OpenRouterSseDecoder, OpenRouterSseFrame, parse_sse_frame_block};

/// Namespace used inside `GenerateContentConfig::extensions` for OpenRouter-native options.
pub const OPENROUTER_EXTENSION_NAMESPACE: &str = "openrouter";
