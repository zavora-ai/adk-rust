use crate::{FinishReason, FunctionCall, GenerationResponse, Model, Part};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[test]
fn test_model_deserialization() {
    #[derive(Serialize, Deserialize)]
    struct Response {
        model: Model,
    }

    let response = Response {
        model: Model::Custom("models/custom_gemini_model".to_string()),
    };
    let serialized = serde_json::to_string(&response).unwrap();
    let deserialized: Response = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.model, response.model);

    let response = Response {
        model: Model::Gemini25Flash,
    };
    let serialized = serde_json::to_string(&response).unwrap();
    let deserialized: Response = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.model, response.model);
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
        Part::FunctionCall {
            function_call,
            thought_signature,
        } => {
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
    assert_eq!(
        function_call.thought_signature,
        Some("test_thought_signature".to_string())
    );

    // Test serialization
    let serialized = serde_json::to_string(&function_call).unwrap();
    println!("Serialized FunctionCall: {}", serialized);

    // Test deserialization
    let deserialized: FunctionCall = serde_json::from_str(&serialized).unwrap();
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
    println!("Serialized FunctionCall without thought: {}", serialized);
    assert!(!serialized.contains("thought_signature"));
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

    // Test serialization of the complete structure first
    let serialized = serde_json::to_string(&model_content).unwrap();
    println!("Serialized multi-turn content: {}", serialized);

    // Verify it contains the thought signature
    assert!(serialized.contains("thoughtSignature"));
    assert!(serialized.contains("sample_thought_signature"));

    let parts = model_content.parts.unwrap();
    assert_eq!(parts.len(), 1);

    match &parts[0] {
        Part::FunctionCall {
            function_call,
            thought_signature,
        } => {
            assert_eq!(function_call.name, "get_weather");
            assert_eq!(
                thought_signature.as_ref().unwrap(),
                "sample_thought_signature"
            );
        }
        _ => panic!("Expected FunctionCall part"),
    }
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
        Part::Text {
            text,
            thought,
            thought_signature,
        } => {
            assert_eq!(*thought, Some(true));
            assert_eq!(*thought_signature, None);
            assert!(text.contains("here's what I'm thinking"));
        }
        _ => panic!("Expected Text part for first element"),
    }

    // Check second part (text with thought signature)
    match &parts[1] {
        Part::Text {
            text,
            thought,
            thought_signature,
        } => {
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
        Part::Text {
            text,
            thought,
            thought_signature,
        } => {
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
        Part::Text {
            text,
            thought,
            thought_signature,
        } => {
            assert_eq!(text, "This is my thinking process");
            assert_eq!(*thought, Some(true));
            assert_eq!(thought_signature.as_ref().unwrap(), "thought_signature_456");
        }
        _ => panic!("Expected Text part"),
    }

    // Test serialization
    let serialized = serde_json::to_string(&content).unwrap();
    println!("Serialized content with thought signature: {}", serialized);
    assert!(serialized.contains("thoughtSignature"));
    assert!(serialized.contains("test_signature_123"));

    // Test serialization of thought content
    let serialized_thought = serde_json::to_string(&thought_content).unwrap();
    println!("Serialized thought content: {}", serialized_thought);
    assert!(serialized_thought.contains("thoughtSignature"));
    assert!(serialized_thought.contains("thought_signature_456"));
    assert!(serialized_thought.contains("\"thought\":true"));
}
