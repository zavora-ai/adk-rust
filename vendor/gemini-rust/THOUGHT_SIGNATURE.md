# thoughtSignature Support

This update adds support for the `thoughtSignature` feature in Gemini 2.5 series models to gemini-rust.

## What is thoughtSignature

`thoughtSignature` is a new feature in Gemini 2.5 series models that provides an encrypted signature of the thinking process when the model makes function calls. This signature represents the model's internal reasoning process when making function call decisions.

## New Features

### 1. FunctionCall Structure Update

The `FunctionCall` structure now includes an optional `thought_signature` field:

```rust
pub struct FunctionCall {
    pub name: String,
    pub args: serde_json::Value,
    pub thought_signature: Option<String>,
}
```

### 1a. Part::Text Structure Update

The `Part::Text` variant now includes an optional `thought_signature` field to support text responses with thought signatures:

```rust
Text {
    text: String,
    thought: Option<bool>,
    thought_signature: Option<String>,  // New field
}
```

### 2. New Constructor Methods

- `FunctionCall::new()` - Creates function call without thought signature (maintains backward compatibility)
- `FunctionCall::with_thought_signature()` - Creates function call with thought signature

```rust
// Without thought signature
let function_call = FunctionCall::new("function_name", args);

// With thought signature
let function_call = FunctionCall::with_thought_signature(
    "function_name", 
    args, 
    "thought_signature_string"
);
```

### 3. Part Enum Update

The `Part::FunctionCall` variant now includes the `thought_signature` field:

```rust
FunctionCall {
    function_call: super::tools::FunctionCall,
    thought_signature: Option<String>,
}
```

### 4. New API Methods

`GenerationResponse` adds methods to retrieve function calls and text with their thought signatures:

```rust
// Get function calls and their corresponding thought signatures
let function_calls_with_thoughts = response.function_calls_with_thoughts();
for (function_call, thought_signature) in function_calls_with_thoughts {
    println!("Function: {}", function_call.name);
    if let Some(signature) = thought_signature {
        println!("Thought Signature: {}", signature);
    }
}

// Get text parts with their thought signatures
let text_with_thoughts = response.text_with_thoughts();
for (text, is_thought, thought_signature) in text_with_thoughts {
    println!("Text: {}", text);
    println!("Is thought: {}", is_thought);
    if let Some(signature) = thought_signature {
        println!("Thought Signature: {}", signature);
    }
}
```

### 5. New Content Creation Methods

`Content` adds methods to create content with thought signatures:

```rust
// Create text content with thought signature
let content = Content::text_with_thought_signature("Response text", "signature123");

// Create thought content with thought signature
let thought_content = Content::thought_with_signature("Thinking process", "signature456");
```

## Usage Examples

Please refer to `examples/thought_signature_example.rs` to learn how to:

1. Enable thinking functionality
2. Retrieve function calls with thought signatures
3. Handle thought signature data
4. **Maintain thinking context across multi-turn conversations**

### Thought Signatures in Multi-turn Conversations

Gemini API text and content generation calls are stateless. When using thinking in multi-turn interactions (such as chat), the model doesn't have access to thought context from previous turns.

You can maintain thought context using thought signatures, which are encrypted representations of the model's internal thought process. When thinking and function calling are enabled, the model returns thought signatures in the response object. To ensure the model maintains context across multiple turns of a conversation, you must provide the thought signatures back to the model in subsequent requests.

#### Important Usage Limitations

1. **Complete Response Preservation**: Return the entire response with all parts containing signatures back to the model
2. **Don't Concatenate Parts**: Don't concatenate parts with signatures together  
3. **Don't Merge Parts**: Don't merge one part with a signature with another part without a signature

#### Example Code

```rust
// First turn: Get function call with thought signature
let response = client
    .generate_content()
    .with_user_message("What's the weather like?")
    .with_tool(weather_tool)
    .with_thinking_config(thinking_config)
    .execute()
    .await?;

// Extract thought signature
let (function_call, thought_signature) = response.function_calls_with_thoughts()[0];

// Second turn: Maintain thinking context
let mut conversation = client.generate_content();

// Important: Include complete model response including thought signature
let model_content = Content {
    parts: Some(vec![Part::FunctionCall {
        function_call: function_call.clone(),
        thought_signature: thought_signature.cloned(), // Key: preserve original signature
    }]),
    role: Some(Role::Model),
};
conversation.contents.push(model_content);

// Continue conversation...
```

## API Response Examples

### Function Call with thoughtSignature

When Gemini 2.5 Pro makes function calls, the response will include `thoughtSignature`:

```json
{
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
            "thoughtSignature": "CtwFAVSoXO4WSz0Ri3HddDzPQzsB8EaYsiQobiBKOzGOaAPM..."
          }
        ],
        "role": "model"
      }
    }
  ]
}
```

### Text Response with thoughtSignature

Text responses can also include `thoughtSignature` fields:

```json
{
  "candidates": [
    {
      "content": {
        "parts": [
          {
            "text": "**Okay, here's what I'm thinking:**\n\nThe user wants me to show them the functions available...",
            "thought": true
          },
          {
            "text": "The following functions are available in the environment: `chat.get_message_count()`",
            "thoughtSignature": "Cs4BA.../Yw="
          }
        ],
        "role": "model"
      }
    }
  ],
  "usageMetadata": {
    "promptTokenCount": 36,
    "candidatesTokenCount": 18,
    "totalTokenCount": 96,
    "thoughtsTokenCount": 42
  }
}
```

## Backward Compatibility

All changes maintain backward compatibility:

- Existing `FunctionCall::new()` method still works
- Existing `function_calls()` method still returns the same results  
- Serialization/deserialization automatically handles missing `thought_signature` fields

## Testing

Three new tests were added to verify functionality:

1. `test_thought_signature_deserialization` - Tests deserialization of responses containing thought signatures
2. `test_function_call_with_thought_signature` - Tests creating function calls with thought signatures
3. `test_function_call_without_thought_signature` - Ensures backward compatibility
4. `test_multi_turn_content_structure` - Tests multi-turn conversation structure

Run tests:

```bash
cargo test test_thought_signature
cargo test test_function_call
cargo test test_multi_turn
```

## Important Notes

- `thoughtSignature` functionality is only available in Gemini 2.5 series models
- Requires enabling thinking functionality to generate thought signatures
- **Must enable function calling simultaneously to receive thought signatures**
- Thought signatures are encrypted strings representing the model's internal reasoning process
- In multi-turn conversations, must return complete responses (with signatures) to the model
- Don't modify, concatenate, or merge parts containing thought signatures
- Thought signatures help the model maintain thinking context across conversation turns
