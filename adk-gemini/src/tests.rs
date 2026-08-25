use crate::{
    BlockReason, FinishReason, FunctionCall, GenerationConfig, GenerationResponse, HarmCategory,
    HarmProbability, Modality, Model, Part, ThinkingConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[test]
fn test_model_deserialization() {
    #[derive(Serialize, Deserialize)]
    struct Response {
        model: Model,
    }

    let response = Response { model: Model::Custom("models/custom_gemini_model".to_string()) };
    let serialized = serde_json::to_string(&response).unwrap();
    let deserialized: Response = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.model, response.model);

    let response = Response { model: Model::Gemini25Flash };
    let serialized = serde_json::to_string(&response).unwrap();
    let deserialized: Response = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.model, response.model);
}

#[test]
fn current_default_uses_future_proof_custom_model() {
    let model = Model::default();
    assert_eq!(model.as_str(), "models/gemini-3.7-flash");
    assert_eq!(Model::gemini_3_6_flash().as_str(), "models/gemini-3.6-flash");
}

#[test]
fn gemini_37_rejects_removed_generation_parameters() {
    use crate::client::validate_generation_config_for_model;

    for config in [
        GenerationConfig { temperature: Some(0.7), ..Default::default() },
        GenerationConfig { top_p: Some(0.9), ..Default::default() },
        GenerationConfig { top_k: Some(20), ..Default::default() },
        GenerationConfig { candidate_count: Some(2), ..Default::default() },
        GenerationConfig {
            thinking_config: Some(ThinkingConfig::new().with_thinking_budget(1024)),
            ..Default::default()
        },
    ] {
        assert!(validate_generation_config_for_model(&Model::default(), &config).is_err());
    }

    assert!(
        validate_generation_config_for_model(
            &Model::default(),
            &GenerationConfig {
                thinking_config: Some(
                    ThinkingConfig::new()
                        .with_thinking_level(crate::generation::ThinkingLevel::Medium),
                ),
                ..Default::default()
            },
        )
        .is_ok()
    );
}

#[cfg(feature = "interactions")]
#[test]
fn gemini_37_interactions_reject_removed_sampling_parameters() {
    use crate::client::validate_interaction_generation_config_for_model;
    use crate::interactions::GenerationConfig as InteractionGenerationConfig;

    let config = InteractionGenerationConfig { temperature: Some(0.7), ..Default::default() };
    let error =
        validate_interaction_generation_config_for_model(Some("models/gemini-3.7-flash"), &config)
            .expect_err("Gemini 3.7 interactions must reject explicit sampling");

    assert!(error.to_string().contains("does not accept temperature or top_p"));
}

#[test]
fn test_may_2026_ga_models_roundtrip() {
    // GA models that shipped/replaced previews in May 2026.
    for (model, wire) in [
        (Model::Gemini35Flash, "models/gemini-3.5-flash"),
        (Model::Gemini31FlashLite, "models/gemini-3.1-flash-lite"),
        (Model::Gemini31FlashImage, "models/gemini-3.1-flash-image"),
        (Model::Gemini3ProImage, "models/gemini-3-pro-image"),
        (Model::GeminiEmbedding2, "models/gemini-embedding-2"),
    ] {
        // as_str matches the wire id
        assert_eq!(model.as_str(), wire);
        // serializes to the wire id
        let serialized = serde_json::to_value(&model).unwrap();
        assert_eq!(serialized, json!(wire));
        // From<String> round-trips both with and without the "models/" prefix
        let bare = wire.strip_prefix("models/").unwrap();
        assert_eq!(Model::from(bare.to_string()), model);
        assert_eq!(Model::from(wire.to_string()), model);
    }
}

#[test]
fn test_function_response_id_strict_matching() {
    use crate::FunctionResponse;

    // No id by default — the field is omitted from the wire payload.
    let resp = FunctionResponse::new("get_weather", json!({"temp": 72}));
    assert_eq!(resp.id, None);
    let json_val = serde_json::to_value(&resp).unwrap();
    assert!(json_val.get("id").is_none());

    // with_id sets the correlation id required by Gemini 3.x strict matching.
    let resp = FunctionResponse::new("get_weather", json!({"temp": 72})).with_id("call_123");
    assert_eq!(resp.id.as_deref(), Some("call_123"));
    let json_val = serde_json::to_value(&resp).unwrap();
    assert_eq!(json_val["id"], json!("call_123"));

    // Round-trips through deserialization.
    let back: FunctionResponse = serde_json::from_value(json_val).unwrap();
    assert_eq!(back, resp);
}

#[test]
fn test_thought_signature_deserialization() {
    // Test JSON that includes thoughtSignature like in the provided API response
    let json_response = json!({
        "candidates": [
            {
                "content": {
                    "parts": [
                        {
                            "functionCall": {
                                "name": "get_current_weather",
                                "args": {
                                    "location": "Kaohsiung Zuoying District"
                                }
                            },
                            "thoughtSignature": "CtwFAVSoXO4WSz0Ri3HddDzPQzsB8EaYsiQobiBKOzGOaAPM0d4DewrzUmhCnZbdboz+n+6v503fcy4epZC2bomn247laY6RHtKTc0UA8scj1DW/Y8w9AsfvjDX1adpIi043qjivTtowjxKAIesKoO69mFj6HTmGRI6sE1hamsIblZGZypowxnBQmxqJftl1aebB7kQN+MoYSeX+OU1z/8G+RXE+cb9cvwdAGIZjHXoGgEaIigYlrjTkZjRGBiI+gC2AcLNe32MHVla2/dmV8O7k8Cl45ksH+4srYABtmXLxjxwQK6s2bjVngvaRcBTCK4AUHiDb1j54n3Fls5J1i9k2sd6OcJYJuRlfwuxv2RMZ+V8zLdNthfSWtZwuJslkOD3uZCkEhO/hI6nAKcyuSokdAKtOw9g6LWORnEQoUJ+BaTVymN1tuJzbzrS9kPP5d3QJfFdQaILkk8CUdnGOEcngvlINN4MGNTQYN+0Au/JFWDWj33T5LZWkbDMp+yIpqFkZuRYwjW/9KOR6qFbxzvJyQcAKTxf0Sq7UfHTYBXTVp0/N4cDWRv+5DF0UOp+6emnPslCmaRK8JEGkmKkYXCzR6PpopfdzHHSDQHbNjjwr0h9ADZKehiB/cB1Jjy0oyBOM3HSHyuzcP8CO4NoAXOUM/VP5P41ys9TdeaPZAZ1E3cGQI4pifFVPdy3o33QSYqS1ce5Wxbeud06+d+sz2O7jJrfHMdgYpcO/2RcXQyK/GVIlDkWyxpYtBZhlkh3vLxPVmV/JJv5DQSS3YNTFSbfbwC8DtrI6YNFK5Vo07cl6mAY+U8b4ziFJk2HGuO27jq5EnhJE6v39HCfXTa8cKaLzpIURJSOs12S1rc3pqXdv4VBL6dp+Yjr8eQPxYRP93QzZMFXcYZ+Vc2H5mbnXbvTxVdYT7Qpu7aK1o6csSOMOx47NzZnOnlTWNJUxtU5UIZJ2JelOt/NsWnVJZY8D"
                        }
                    ],
                    "role": "model"
                },
                "finishReason": "STOP",
                "index": 0
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 70,
            "candidatesTokenCount": 21,
            "totalTokenCount": 255,
            "thoughtsTokenCount": 164
        },
        "modelVersion": "gemini-2.5-pro",
        "responseId": "CCm8aJjzBaWh1MkP_cLEgQo"
    });

    // Test deserialization
    let response: GenerationResponse = serde_json::from_value(json_response).unwrap();

    // Verify basic structure
    assert_eq!(response.candidates.len(), 1);
    let candidate = &response.candidates[0];
    assert_eq!(candidate.finish_reason, Some(FinishReason::Stop));

    // Check content parts
    let parts = candidate.content.parts.as_ref().unwrap();
    assert_eq!(parts.len(), 1);

    // Verify the part is a function call with thought signature
    match &parts[0] {
        Part::FunctionCall { function_call, thought_signature } => {
            assert_eq!(function_call.name, "get_current_weather");
            assert_eq!(function_call.args["location"], "Kaohsiung Zuoying District");

            // Verify thought signature is present and not empty
            assert!(thought_signature.is_some());
            let signature = thought_signature.as_ref().unwrap();
            assert!(!signature.is_empty());
            assert!(signature.starts_with("CtwFAVSoXO4WSz0Ri3HddDzPQzsB8EaYsiQobiBKOzGOaAPM"));
        }
        _ => panic!("Expected FunctionCall part"),
    }

    // Test the function_calls_with_thoughts method
    let function_calls_with_thoughts = response.function_calls_with_thoughts();
    assert_eq!(function_calls_with_thoughts.len(), 1);

    let (function_call, thought_signature) = &function_calls_with_thoughts[0];
    assert_eq!(function_call.name, "get_current_weather");
    assert!(thought_signature.is_some());

    // Test usage metadata with thinking tokens
    assert!(response.usage_metadata.is_some());
    let usage = response.usage_metadata.as_ref().unwrap();
    assert_eq!(usage.thoughts_token_count, Some(164));
}

#[test]
fn test_function_call_with_thought_signature() {
    // Test creating a FunctionCall with thought signature
    let function_call = FunctionCall::with_thought_signature(
        "test_function",
        json!({"param": "value"}),
        "test_thought_signature",
    );

    assert_eq!(function_call.name, "test_function");
    assert_eq!(function_call.args["param"], "value");
    assert_eq!(function_call.thought_signature, Some("test_thought_signature".to_string()));

    // thoughtSignature is now serialized to support Gemini 3.x multi-turn tool calling.
    let serialized = serde_json::to_value(&function_call).unwrap();
    assert_eq!(
        serialized,
        json!({
            "name": "test_function",
            "args": {"param": "value"},
            "thoughtSignature": "test_thought_signature"
        })
    );

    // Accept both camelCase and legacy snake_case on input.
    let deserialized: FunctionCall = serde_json::from_value(json!({
        "name": "test_function",
        "args": {"param": "value"},
        "thoughtSignature": "test_thought_signature"
    }))
    .unwrap();
    assert_eq!(deserialized, function_call);
}

#[test]
fn test_function_call_without_thought_signature() {
    // Test creating a FunctionCall without thought signature (backward compatibility)
    let function_call = FunctionCall::new("test_function", json!({"param": "value"}));

    assert_eq!(function_call.name, "test_function");
    assert_eq!(function_call.args["param"], "value");
    assert_eq!(function_call.thought_signature, None);

    // Test serialization should not include thought_signature field when None
    let serialized = serde_json::to_string(&function_call).unwrap();
    assert!(!serialized.contains("thought_signature"));
    assert!(!serialized.contains("thoughtSignature"));
}

#[test]
fn test_multi_turn_content_structure() {
    // Test that we can create proper multi-turn content structure for maintaining thought context
    use crate::{Content, Part, Role};

    // Simulate a function call with thought signature from first turn
    let function_call = FunctionCall::with_thought_signature(
        "get_weather",
        json!({"location": "Tokyo"}),
        "sample_thought_signature",
    );

    // Create model content with function call and thought signature
    let model_content = Content {
        parts: Some(vec![Part::FunctionCall {
            function_call: function_call.clone(),
            thought_signature: Some("sample_thought_signature".to_string()),
        }]),
        role: Some(Role::Model),
    };

    // Verify structure
    assert!(model_content.parts.is_some());
    assert_eq!(model_content.role, Some(Role::Model));

    // thoughtSignature is now serialized to support Gemini 3.x multi-turn tool calling.
    let serialized = serde_json::to_value(&model_content).unwrap();
    assert_eq!(
        serialized,
        json!({
            "parts": [
                {
                    "functionCall": {
                        "name": "get_weather",
                        "args": {"location": "Tokyo"},
                        "thoughtSignature": "sample_thought_signature"
                    },
                    "thoughtSignature": "sample_thought_signature"
                }
            ],
            "role": "model"
        })
    );

    let parts = model_content.parts.unwrap();
    assert_eq!(parts.len(), 1);

    match &parts[0] {
        Part::FunctionCall { function_call, thought_signature } => {
            assert_eq!(function_call.name, "get_weather");
            assert_eq!(thought_signature.as_ref().unwrap(), "sample_thought_signature");
        }
        _ => panic!("Expected FunctionCall part"),
    }
}

#[test]
fn test_function_response_wraps_array_payloads() {
    use crate::Content;

    let content = Content::function_response_json("rag_search", json!([{ "id": "pricing" }]));
    let serialized = serde_json::to_value(&content).unwrap();

    assert_eq!(
        serialized,
        json!({
            "parts": [
                {
                    "functionResponse": {
                        "name": "rag_search",
                        "response": {
                            "result": [
                                { "id": "pricing" }
                            ]
                        }
                    }
                }
            ]
        })
    );
}

#[test]
fn test_text_with_thought_signature() {
    use crate::GenerationResponse;

    // Test JSON similar to the provided API response
    let json_response = json!({
        "candidates": [
            {
                "content": {
                    "parts": [
                        {
                            "text": "**Okay, here's what I'm thinking:**\n\nThe user wants me to show them...",
                            "thought": true
                        },
                        {
                            "text": "The following functions are available in the environment: `chat.get_message_count()`",
                            "thoughtSignature": "Cs4BA.../Yw="
                        }
                    ],
                    "role": "model"
                },
                "finishReason": "STOP",
                "index": 0
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 36,
            "candidatesTokenCount": 18,
            "totalTokenCount": 96,
            "thoughtsTokenCount": 42
        },
        "modelVersion": "gemini-2.5-flash",
        "responseId": "gIC..."
    });

    // Test deserialization
    let response: GenerationResponse = serde_json::from_value(json_response).unwrap();

    // Verify basic structure
    assert_eq!(response.candidates.len(), 1);
    let candidate = &response.candidates[0];

    // Check content parts
    let parts = candidate.content.parts.as_ref().unwrap();
    assert_eq!(parts.len(), 2);

    // Check first part (thought without signature)
    match &parts[0] {
        Part::Text { text, thought, thought_signature } => {
            assert_eq!(*thought, Some(true));
            assert_eq!(*thought_signature, None);
            assert!(text.contains("here's what I'm thinking"));
        }
        _ => panic!("Expected Text part for first element"),
    }

    // Check second part (text with thought signature)
    match &parts[1] {
        Part::Text { text, thought, thought_signature } => {
            assert_eq!(*thought, None);
            assert!(thought_signature.is_some());
            assert_eq!(thought_signature.as_ref().unwrap(), "Cs4BA.../Yw=");
            assert!(text.contains("chat.get_message_count"));
        }
        _ => panic!("Expected Text part for second element"),
    }

    // Test the new text_with_thoughts method
    let text_with_thoughts = response.text_with_thoughts();
    assert_eq!(text_with_thoughts.len(), 2);

    let (first_text, is_thought, thought_sig) = &text_with_thoughts[0];
    assert!(*is_thought);
    assert!(thought_sig.is_none());
    assert!(first_text.contains("here's what I'm thinking"));

    let (second_text, is_thought, thought_sig) = &text_with_thoughts[1];
    assert!(!(*is_thought));
    assert!(thought_sig.is_some());
    assert_eq!(thought_sig.unwrap(), "Cs4BA.../Yw=");
    assert!(second_text.contains("chat.get_message_count"));
}

#[test]
fn test_content_creation_with_thought_signature() {
    // Test creating content with thought signature
    use crate::Content;
    let content = Content::text_with_thought_signature("Test response", "test_signature_123");

    let parts = content.parts.as_ref().unwrap();
    assert_eq!(parts.len(), 1);

    match &parts[0] {
        Part::Text { text, thought, thought_signature } => {
            assert_eq!(text, "Test response");
            assert_eq!(*thought, None);
            assert_eq!(thought_signature.as_ref().unwrap(), "test_signature_123");
        }
        _ => panic!("Expected Text part"),
    }

    // Test creating thought content with signature
    let thought_content =
        Content::thought_with_signature("This is my thinking process", "thought_signature_456");

    let parts = thought_content.parts.as_ref().unwrap();
    assert_eq!(parts.len(), 1);

    match &parts[0] {
        Part::Text { text, thought, thought_signature } => {
            assert_eq!(text, "This is my thinking process");
            assert_eq!(*thought, Some(true));
            assert_eq!(thought_signature.as_ref().unwrap(), "thought_signature_456");
        }
        _ => panic!("Expected Text part"),
    }
    // thoughtSignature is now serialized when present (Gemini 3.x support).
    let serialized = serde_json::to_string(&content).unwrap();
    assert!(serialized.contains("thoughtSignature"));

    // thought field IS serialized, and thoughtSignature IS serialized when present.
    let serialized_thought = serde_json::to_string(&thought_content).unwrap();
    assert!(serialized_thought.contains("thoughtSignature"));
    assert!(serialized_thought.contains("\"thought\":true"));
}

#[test]
fn test_vertex_numeric_enum_deserialization() {
    let finish_reason: FinishReason = serde_json::from_value(json!(1)).unwrap();
    assert_eq!(finish_reason, FinishReason::Stop);

    let block_reason: BlockReason = serde_json::from_value(json!(5)).unwrap();
    assert_eq!(block_reason, BlockReason::ModelArmor);

    let harm_category: HarmCategory = serde_json::from_value(json!(1)).unwrap();
    assert_eq!(harm_category, HarmCategory::HateSpeech);

    let harm_probability: HarmProbability = serde_json::from_value(json!(3)).unwrap();
    assert_eq!(harm_probability, HarmProbability::Medium);

    let modality: Modality = serde_json::from_value(json!(4)).unwrap();
    assert_eq!(modality, Modality::Audio);
}

#[test]
fn test_thinking_level_serialization() {
    use crate::ThinkingLevel;

    // ThinkingLevel serializes as lowercase
    let level = ThinkingLevel::Low;
    let json = serde_json::to_value(level).unwrap();
    assert_eq!(json, json!("low"));

    let level = ThinkingLevel::High;
    let json = serde_json::to_value(level).unwrap();
    assert_eq!(json, json!("high"));

    // Round-trip all variants
    for (variant, expected) in [
        (ThinkingLevel::Minimal, "minimal"),
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::Medium, "medium"),
        (ThinkingLevel::High, "high"),
    ] {
        let serialized = serde_json::to_string(&variant).unwrap();
        assert_eq!(serialized, format!("\"{expected}\""));
        let deserialized: ThinkingLevel = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, variant);
    }
}

#[test]
fn test_thinking_config_with_level() {
    use crate::{ThinkingConfig, ThinkingLevel};

    // Builder method sets thinking_level
    let config = ThinkingConfig::new()
        .with_thinking_level(ThinkingLevel::Medium)
        .with_thoughts_included(true);

    assert_eq!(config.thinking_level, Some(ThinkingLevel::Medium));
    assert_eq!(config.include_thoughts, Some(true));
    assert_eq!(config.thinking_budget, None);

    // Serializes correctly with camelCase
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["thinkingLevel"], json!("medium"));
    assert_eq!(json["includeThoughts"], json!(true));
    assert!(json.get("thinkingBudget").is_none());
}

#[test]
fn test_thinking_config_budget_and_level_independent() {
    use crate::{ThinkingConfig, ThinkingLevel};

    // Budget-only config (Gemini 2.5 style)
    let budget_config = ThinkingConfig::new().with_thinking_budget(2048);
    let json = serde_json::to_value(&budget_config).unwrap();
    assert_eq!(json["thinkingBudget"], json!(2048));
    assert!(json.get("thinkingLevel").is_none());

    // Level-only config (Gemini 3 style)
    let level_config = ThinkingConfig::new().with_thinking_level(ThinkingLevel::High);
    let json = serde_json::to_value(&level_config).unwrap();
    assert_eq!(json["thinkingLevel"], json!("high"));
    assert!(json.get("thinkingBudget").is_none());
}
