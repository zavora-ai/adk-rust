//! Property-based tests for MCP Sampling conversion functions.
//!
//! These tests verify that converting between MCP sampling types and ADK LLM types
//! preserves all relevant fields.

#![cfg(feature = "mcp-sampling")]

use adk_core::model::{FinishReason, LlmResponse};
use adk_core::types::{Content, Part};
use adk_tool::sampling::{
    SamplingContent, SamplingMessage, SamplingRequest, llm_response_to_sampling_response,
    sampling_request_to_llm_request,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generate arbitrary text content for sampling messages.
fn arb_sampling_text_content() -> impl Strategy<Value = SamplingContent> {
    "[a-zA-Z0-9 .,!?]{0,200}".prop_map(|text| SamplingContent::Text { text })
}

/// Generate an arbitrary sampling message with a role and text content.
fn arb_sampling_message() -> impl Strategy<Value = SamplingMessage> {
    (
        prop_oneof![Just("user".to_string()), Just("assistant".to_string())],
        arb_sampling_text_content(),
    )
        .prop_map(|(role, content)| SamplingMessage { role, content })
}

/// Generate an arbitrary SamplingRequest with random messages, system prompt,
/// max_tokens, and temperature.
fn arb_sampling_request() -> impl Strategy<Value = SamplingRequest> {
    (
        prop::collection::vec(arb_sampling_message(), 1..=10),
        prop::option::of("[a-zA-Z0-9 .,!?]{1,100}".prop_map(String::from)),
        prop::option::of(1u32..=10_000u32),
        prop::option::of((0u32..=20u32).prop_map(|v| v as f64 / 10.0)),
    )
        .prop_map(|(messages, system_prompt, max_tokens, temperature)| SamplingRequest {
            messages,
            system_prompt,
            model_preferences: None,
            max_tokens,
            temperature,
        })
}

/// Generate an arbitrary FinishReason.
fn arb_finish_reason() -> impl Strategy<Value = FinishReason> {
    prop_oneof![
        Just(FinishReason::Stop),
        Just(FinishReason::MaxTokens),
        Just(FinishReason::Safety),
        Just(FinishReason::Recitation),
        Just(FinishReason::Other),
    ]
}

/// Generate an arbitrary LlmResponse with text content, a finish reason,
/// and no error.
fn arb_llm_response() -> impl Strategy<Value = LlmResponse> {
    ("[a-zA-Z0-9 .,!?]{0,200}", prop::option::of(arb_finish_reason())).prop_map(
        |(text, finish_reason)| LlmResponse {
            content: Some(Content::new("model").with_text(text)),
            finish_reason,
            ..Default::default()
        },
    )
}

/// Generate an arbitrary model name string.
fn arb_model_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9-]{2,30}"
}

// ---------------------------------------------------------------------------
// Property 5: MCP Sampling Request Conversion Preserves Fields
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]

    /// **Feature: competitive-parity-v070, Property 5: MCP Sampling Request Conversion Preserves Fields**
    ///
    /// *For any* SamplingRequest with messages, system prompt, max tokens, and temperature,
    /// converting to an LlmRequest SHALL preserve: the number and content of messages,
    /// the system prompt text, the max tokens value, and the temperature value.
    ///
    /// **Validates: Requirements 4.2**
    #[test]
    fn prop_sampling_request_conversion_preserves_fields(
        request in arb_sampling_request(),
        model_name in arb_model_name(),
    ) {
        let llm_req = sampling_request_to_llm_request(&request, &model_name);

        // --- Model name ---
        prop_assert_eq!(&llm_req.model, &model_name);

        // --- Message count ---
        // If system_prompt is present, it adds one extra Content entry at the front.
        let expected_content_count = request.messages.len()
            + if request.system_prompt.is_some() { 1 } else { 0 };
        prop_assert_eq!(
            llm_req.contents.len(),
            expected_content_count,
            "content count mismatch: expected {} (messages={}, system_prompt={})",
            expected_content_count,
            request.messages.len(),
            request.system_prompt.is_some()
        );

        // --- System prompt preservation ---
        let offset = if let Some(ref sys) = request.system_prompt {
            let first = &llm_req.contents[0];
            prop_assert_eq!(&first.role, "system");
            // Extract text from the first part
            match &first.parts[0] {
                Part::Text { text } => prop_assert_eq!(text, sys),
                other => prop_assert!(false, "expected Text part, got {:?}", other),
            }
            1
        } else {
            0
        };

        // --- Message content preservation ---
        for (i, msg) in request.messages.iter().enumerate() {
            let content = &llm_req.contents[offset + i];

            // Role mapping: "assistant" → "model", others unchanged
            let expected_role = match msg.role.as_str() {
                "assistant" => "model",
                other => other,
            };
            prop_assert_eq!(&content.role, expected_role);

            // Text content preserved
            if let SamplingContent::Text { ref text } = msg.content {
                match &content.parts[0] {
                    Part::Text { text: llm_text } => prop_assert_eq!(llm_text, text),
                    other => prop_assert!(false, "expected Text part, got {:?}", other),
                }
            }
        }

        // --- Config preservation ---
        let config = llm_req.config.as_ref().expect("config should be present");

        // max_tokens → max_output_tokens
        prop_assert_eq!(
            config.max_output_tokens,
            request.max_tokens.map(|t| t as i32)
        );

        // temperature (f64 → f32, check approximate equality)
        match (request.temperature, config.temperature) {
            (Some(req_temp), Some(cfg_temp)) => {
                let diff = (req_temp as f32 - cfg_temp).abs();
                prop_assert!(
                    diff < 0.01,
                    "temperature mismatch: request={}, config={}",
                    req_temp,
                    cfg_temp
                );
            }
            (None, None) => {} // both None, OK
            (req, cfg) => {
                prop_assert!(
                    false,
                    "temperature mismatch: request={:?}, config={:?}",
                    req,
                    cfg
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property 6: MCP Sampling Response Conversion Preserves Fields
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]

    /// **Feature: competitive-parity-v070, Property 6: MCP Sampling Response Conversion Preserves Fields**
    ///
    /// *For any* LlmResponse with content text, model identifier, and finish reason,
    /// converting to a SamplingResponse SHALL preserve the content text, include the
    /// model identifier, and map the finish reason to the correct MCP stop reason string.
    ///
    /// **Validates: Requirements 4.3**
    #[test]
    fn prop_sampling_response_conversion_preserves_fields(
        llm_resp in arb_llm_response(),
        model_name in arb_model_name(),
    ) {
        // Capture expected values before conversion
        let expected_text = llm_resp
            .content
            .as_ref()
            .and_then(|c| {
                c.parts.iter().find_map(|p| match p {
                    Part::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default();

        let expected_stop_reason = match llm_resp.finish_reason {
            Some(FinishReason::Stop) => "endTurn",
            Some(FinishReason::MaxTokens) => "maxTokens",
            Some(FinishReason::Safety) => "safety",
            Some(FinishReason::Recitation) => "recitation",
            Some(FinishReason::Other) => "other",
            None => "endTurn",
        };

        let sampling_resp = llm_response_to_sampling_response(llm_resp, &model_name);

        // --- Model identifier preserved ---
        prop_assert_eq!(&sampling_resp.model, &model_name);

        // --- Content text preserved ---
        match &sampling_resp.content {
            SamplingContent::Text { text } => {
                prop_assert_eq!(text, &expected_text);
            }
            other => {
                prop_assert!(false, "expected Text content, got {:?}", other);
            }
        }

        // --- Finish reason mapped correctly ---
        prop_assert_eq!(&sampling_resp.stop_reason, expected_stop_reason);
    }

    /// Additional property: LlmResponse with None content produces empty text.
    ///
    /// **Validates: Requirements 4.3**
    #[test]
    fn prop_sampling_response_none_content_produces_empty_text(
        finish_reason in prop::option::of(arb_finish_reason()),
        model_name in arb_model_name(),
    ) {
        let llm_resp = LlmResponse {
            content: None,
            finish_reason,
            ..Default::default()
        };

        let sampling_resp = llm_response_to_sampling_response(llm_resp, &model_name);

        match &sampling_resp.content {
            SamplingContent::Text { text } => {
                prop_assert_eq!(text, "");
            }
            other => {
                prop_assert!(false, "expected empty Text content, got {:?}", other);
            }
        }
    }
}
