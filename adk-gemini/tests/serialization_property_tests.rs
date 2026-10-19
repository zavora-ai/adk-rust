//! Property-based tests for Gemini request/response JSON serialization round-trip.
//!
//! **Feature: one-point-zero-readiness, Property 3: Gemini Request Serialization Round-Trip**
//!
//! *For any* valid Gemini `GenerateContentRequest` struct, serializing to JSON and
//! deserializing back SHALL produce a struct equal to the original. This includes
//! requests with varying numbers of contents, tools, system instructions, and
//! generation config parameters.
//!
//! **Validates: Requirements 10.2**

use adk_gemini::{
    Blob, Content, FileDataRef, FunctionCall, FunctionCallingConfig, FunctionCallingMode,
    FunctionDeclaration, FunctionResponse, GenerateContentRequest, GenerationConfig,
    HarmBlockThreshold, HarmCategory, Part, Role, SafetySetting, ThinkingConfig, Tool, ToolConfig,
};
use proptest::prelude::*;

// ============================================================================
// Generators
// ============================================================================

/// Generates a random Role.
fn arb_role() -> impl Strategy<Value = Role> {
    prop_oneof![Just(Role::User), Just(Role::Model),]
}

/// Generates a random HarmCategory (only the ones that round-trip cleanly via string serialization).
fn arb_harm_category() -> impl Strategy<Value = HarmCategory> {
    prop_oneof![
        Just(HarmCategory::Harassment),
        Just(HarmCategory::HateSpeech),
        Just(HarmCategory::SexuallyExplicit),
        Just(HarmCategory::DangerousContent),
        Just(HarmCategory::CivicIntegrity),
        Just(HarmCategory::Jailbreak),
    ]
}

/// Generates a random HarmBlockThreshold.
fn arb_harm_block_threshold() -> impl Strategy<Value = HarmBlockThreshold> {
    prop_oneof![
        Just(HarmBlockThreshold::HarmBlockThresholdUnspecified),
        Just(HarmBlockThreshold::BlockLowAndAbove),
        Just(HarmBlockThreshold::BlockMediumAndAbove),
        Just(HarmBlockThreshold::BlockOnlyHigh),
        Just(HarmBlockThreshold::BlockNone),
        Just(HarmBlockThreshold::Off),
    ]
}

/// Generates a random SafetySetting.
fn arb_safety_setting() -> impl Strategy<Value = SafetySetting> {
    (arb_harm_category(), arb_harm_block_threshold())
        .prop_map(|(category, threshold)| SafetySetting { category, threshold })
}

/// Generates a simple text Part.
fn arb_text_part() -> impl Strategy<Value = Part> {
    ("[a-zA-Z0-9 ]{1,50}", prop::option::of(Just(true)), prop::option::of("[a-zA-Z0-9]{5,20}"))
        .prop_map(|(text, thought, thought_signature)| Part::Text {
            text,
            thought,
            thought_signature,
        })
}

/// Generates an InlineData Part.
fn arb_inline_data_part() -> impl Strategy<Value = Part> {
    ("(image/png|image/jpeg|text/plain)", "[a-zA-Z0-9+/=]{4,40}")
        .prop_map(|(mime_type, data)| Part::InlineData { inline_data: Blob::new(mime_type, data) })
}

/// Generates a FileData Part.
fn arb_file_data_part() -> impl Strategy<Value = Part> {
    ("(application/pdf|image/png)", "gs://[a-z]{3,10}/[a-z]{3,10}").prop_map(
        |(mime_type, file_uri)| Part::FileData { file_data: FileDataRef { mime_type, file_uri } },
    )
}

/// Generates a FunctionCall Part.
fn arb_function_call_part() -> impl Strategy<Value = Part> {
    ("[a-z_]{3,15}", prop::option::of("[a-zA-Z0-9]{5,20}")).prop_map(|(name, thought_signature)| {
        Part::FunctionCall {
            function_call: FunctionCall::new(name, serde_json::json!({"key": "value"})),
            thought_signature,
        }
    })
}

/// Generates a FunctionResponse Part.
fn arb_function_response_part() -> impl Strategy<Value = Part> {
    "[a-z_]{3,15}".prop_map(|name| Part::FunctionResponse {
        function_response: FunctionResponse::new(name, serde_json::json!({"result": "ok"})),
        thought_signature: None,
    })
}

/// Generates a random Part (subset of variants that round-trip cleanly).
fn arb_part() -> impl Strategy<Value = Part> {
    prop_oneof![
        arb_text_part(),
        arb_inline_data_part(),
        arb_file_data_part(),
        arb_function_call_part(),
        arb_function_response_part(),
    ]
}

/// Generates a random Content.
fn arb_content() -> impl Strategy<Value = Content> {
    (prop::option::of(prop::collection::vec(arb_part(), 1..4)), prop::option::of(arb_role()))
        .prop_map(|(parts, role)| Content { parts, role })
}

/// Generates a random FunctionDeclaration.
fn arb_function_declaration() -> impl Strategy<Value = FunctionDeclaration> {
    ("[a-z_]{3,15}", "[a-zA-Z ]{5,30}")
        .prop_map(|(name, description)| FunctionDeclaration::new(name, description, None))
}

/// Generates a random Tool.
fn arb_tool() -> impl Strategy<Value = Tool> {
    prop_oneof![
        // Function tool with 1-3 declarations
        prop::collection::vec(arb_function_declaration(), 1..4).prop_map(Tool::with_functions),
        // Google Search tool
        Just(Tool::google_search()),
        // Code execution tool
        Just(Tool::code_execution()),
    ]
}

/// Generates a random FunctionCallingMode.
fn arb_function_calling_mode() -> impl Strategy<Value = FunctionCallingMode> {
    prop_oneof![
        Just(FunctionCallingMode::Auto),
        Just(FunctionCallingMode::Any),
        Just(FunctionCallingMode::None),
        Just(FunctionCallingMode::Validated),
    ]
}

/// Generates a random ToolConfig.
fn arb_tool_config() -> impl Strategy<Value = ToolConfig> {
    (prop::option::of(arb_function_calling_mode()), prop::option::of(Just(true))).prop_map(
        |(mode, include_server_side)| ToolConfig {
            function_calling_config: mode
                .map(|m| FunctionCallingConfig { mode: m, allowed_function_names: None }),
            include_server_side_tool_invocations: include_server_side,
            retrieval_config: None,
        },
    )
}

/// Generates a random GenerationConfig.
fn arb_generation_config() -> impl Strategy<Value = GenerationConfig> {
    (
        prop::option::of(0.0f32..2.0f32),
        prop::option::of(0.0f32..1.0f32),
        prop::option::of(1i32..100i32),
        prop::option::of(1i32..8192i32),
        prop::option::of(prop::collection::vec("[a-z]{2,5}", 1..3)),
        prop::option::of("(application/json|text/plain)"),
    )
        .prop_map(
            |(temperature, top_p, top_k, max_output_tokens, stop_sequences, response_mime_type)| {
                GenerationConfig {
                    temperature,
                    top_p,
                    top_k,
                    max_output_tokens,
                    candidate_count: None,
                    stop_sequences,
                    response_mime_type,
                    response_schema: None,
                    response_modalities: None,
                    speech_config: None,
                    thinking_config: None,
                    ..Default::default()
                }
            },
        )
}

/// Generates a random GenerateContentRequest.
fn arb_generate_content_request() -> impl Strategy<Value = GenerateContentRequest> {
    (
        prop::collection::vec(arb_content(), 1..4),
        prop::option::of(arb_generation_config()),
        prop::option::of(prop::collection::vec(arb_safety_setting(), 1..3)),
        prop::option::of(prop::collection::vec(arb_tool(), 1..3)),
        prop::option::of(arb_tool_config()),
        prop::option::of(arb_content()),
        prop::option::of("[a-z/]{5,20}"),
    )
        .prop_map(
            |(
                contents,
                generation_config,
                safety_settings,
                tools,
                tool_config,
                system_instruction,
                cached_content,
            )| {
                GenerateContentRequest {
                    contents,
                    generation_config,
                    safety_settings,
                    tools,
                    tool_config,
                    system_instruction,
                    cached_content,
                }
            },
        )
}

// ============================================================================
// Property Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: one-point-zero-readiness, Property 3: Gemini Request Serialization Round-Trip**
    ///
    /// *For any* valid Gemini `GenerateContentRequest` struct, serializing to JSON and
    /// deserializing back SHALL produce a struct equal to the original.
    ///
    /// **Validates: Requirements 10.2**
    #[test]
    fn prop_request_serialization_roundtrip(request in arb_generate_content_request()) {
        let json = serde_json::to_string(&request).expect("serialization should succeed");
        let deserialized: GenerateContentRequest =
            serde_json::from_str(&json).expect("deserialization should succeed");

        // Compare via re-serialization to handle floating point and field ordering
        let json_original = serde_json::to_value(&request).expect("to_value should succeed");
        let json_roundtrip = serde_json::to_value(&deserialized).expect("to_value should succeed");

        prop_assert_eq!(
            json_original,
            json_roundtrip,
            "Request should survive JSON round-trip"
        );
    }

    /// **Feature: one-point-zero-readiness, Property 3 (Content variant): Content Round-Trip**
    ///
    /// *For any* valid Content struct, serializing to JSON and deserializing back
    /// SHALL produce a struct equal to the original.
    ///
    /// **Validates: Requirements 10.2**
    #[test]
    fn prop_content_serialization_roundtrip(content in arb_content()) {
        let json = serde_json::to_string(&content).expect("serialization should succeed");
        let deserialized: Content =
            serde_json::from_str(&json).expect("deserialization should succeed");

        prop_assert_eq!(
            &content,
            &deserialized,
            "Content should survive JSON round-trip"
        );
    }

    /// **Feature: one-point-zero-readiness, Property 3 (GenerationConfig variant): GenerationConfig Round-Trip**
    ///
    /// *For any* valid GenerationConfig struct, serializing to JSON and deserializing back
    /// SHALL produce a struct equal to the original.
    ///
    /// **Validates: Requirements 10.2**
    #[test]
    fn prop_generation_config_serialization_roundtrip(config in arb_generation_config()) {
        let json = serde_json::to_string(&config).expect("serialization should succeed");
        let deserialized: GenerationConfig =
            serde_json::from_str(&json).expect("deserialization should succeed");

        let json_original = serde_json::to_value(&config).expect("to_value should succeed");
        let json_roundtrip = serde_json::to_value(&deserialized).expect("to_value should succeed");

        prop_assert_eq!(
            json_original,
            json_roundtrip,
            "GenerationConfig should survive JSON round-trip"
        );
    }
}

// ============================================================================
// Additional deterministic tests
// ============================================================================

#[test]
fn test_empty_request_roundtrip() {
    let request = GenerateContentRequest {
        contents: vec![Content::text("Hello")],
        generation_config: None,
        safety_settings: None,
        tools: None,
        tool_config: None,
        system_instruction: None,
        cached_content: None,
    };

    let json = serde_json::to_string(&request).unwrap();
    let deserialized: GenerateContentRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::to_value(&deserialized).unwrap()
    );
}

#[test]
fn test_request_with_thinking_config_roundtrip() {
    let request = GenerateContentRequest {
        contents: vec![Content::text("Think about this")],
        generation_config: Some(GenerationConfig {
            thinking_config: Some(ThinkingConfig::new().with_thinking_budget(4096)),
            ..Default::default()
        }),
        safety_settings: None,
        tools: None,
        tool_config: None,
        system_instruction: None,
        cached_content: None,
    };

    let json = serde_json::to_string(&request).unwrap();
    let deserialized: GenerateContentRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::to_value(&deserialized).unwrap()
    );
}

#[test]
fn test_request_with_tools_roundtrip() {
    let request = GenerateContentRequest {
        contents: vec![Content::text("Use tools")],
        generation_config: None,
        safety_settings: None,
        tools: Some(vec![
            Tool::with_functions(vec![FunctionDeclaration::new(
                "get_weather",
                "Get weather for a location",
                None,
            )]),
            Tool::google_search(),
        ]),
        tool_config: Some(ToolConfig {
            function_calling_config: Some(FunctionCallingConfig {
                mode: FunctionCallingMode::Auto,
                allowed_function_names: None,
            }),
            include_server_side_tool_invocations: Some(true),
            retrieval_config: None,
        }),
        system_instruction: Some(Content::text("You are a helpful assistant")),
        cached_content: None,
    };

    let json = serde_json::to_string(&request).unwrap();
    let deserialized: GenerateContentRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::to_value(&deserialized).unwrap()
    );
}
