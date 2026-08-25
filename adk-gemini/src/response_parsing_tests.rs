//! Rigorous response parsing tests for the Gemini API.
//!
//! These tests validate that real-world JSON responses from both AI Studio
//! and Vertex AI backends deserialize correctly into our types, covering
//! edge cases like numeric enums, missing fields, streaming chunks,
//! blocked prompts, grounding metadata, and mixed part types.

use crate::{
    BlockReason, FinishReason, GenerationResponse, HarmCategory, HarmProbability, Modality, Part,
    SafetyRating,
};
use serde_json::json;

// ── Basic text response ─────────────────────────────────────────────

#[test]
fn parse_simple_text_response() {
    let json = json!({
        "candidates": [{
            "content": {
                "parts": [{"text": "Hello, world!"}],
                "role": "model"
            },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": 5,
            "candidatesTokenCount": 4,
            "totalTokenCount": 9
        },
        "modelVersion": "gemini-2.5-flash",
        "responseId": "abc123"
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.text(), "Hello, world!");
    assert_eq!(resp.candidates.len(), 1);
    assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Stop));
    assert_eq!(resp.model_version.as_deref(), Some("gemini-2.5-flash"));
    assert_eq!(resp.response_id.as_deref(), Some("abc123"));

    let usage = resp.usage_metadata.as_ref().unwrap();
    assert_eq!(usage.prompt_token_count, Some(5));
    assert_eq!(usage.candidates_token_count, Some(4));
    assert_eq!(usage.total_token_count, Some(9));
    assert_eq!(usage.thoughts_token_count, None);
}

// ── Multi-candidate response ────────────────────────────────────────

#[test]
fn parse_multi_candidate_response() {
    let json = json!({
        "candidates": [
            {
                "content": {"parts": [{"text": "Answer A"}], "role": "model"},
                "finishReason": "STOP",
                "index": 0
            },
            {
                "content": {"parts": [{"text": "Answer B"}], "role": "model"},
                "finishReason": "STOP",
                "index": 1
            }
        ]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.candidates.len(), 2);
    assert_eq!(resp.text(), "Answer A"); // text() returns first candidate
    assert_eq!(resp.candidates[1].index, Some(1));
}

// ── Safety ratings (string format — AI Studio) ──────────────────────

#[test]
fn parse_response_with_safety_ratings_string() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "Safe response"}], "role": "model"},
            "finishReason": "STOP",
            "safetyRatings": [
                {"category": "HARM_CATEGORY_HATE_SPEECH", "probability": "NEGLIGIBLE"},
                {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "probability": "LOW"},
                {"category": "HARM_CATEGORY_HARASSMENT", "probability": "NEGLIGIBLE"},
                {"category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "probability": "NEGLIGIBLE"}
            ]
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let ratings = resp.candidates[0].safety_ratings.as_ref().unwrap();
    assert_eq!(ratings.len(), 4);
    assert_eq!(ratings[0].category, HarmCategory::HateSpeech);
    assert_eq!(ratings[0].probability, HarmProbability::Negligible);
    assert_eq!(ratings[1].category, HarmCategory::DangerousContent);
    assert_eq!(ratings[1].probability, HarmProbability::Low);
}

// ── Safety ratings (numeric format — Vertex AI) ─────────────────────

#[test]
fn parse_response_with_safety_ratings_numeric() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "Vertex response"}], "role": "model"},
            "finishReason": 1,
            "safetyRatings": [
                {"category": 1, "probability": 1},
                {"category": 2, "probability": 2},
                {"category": 3, "probability": 3},
                {"category": 4, "probability": 1}
            ]
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Stop));

    let ratings = resp.candidates[0].safety_ratings.as_ref().unwrap();
    assert_eq!(ratings[0].category, HarmCategory::HateSpeech);
    assert_eq!(ratings[0].probability, HarmProbability::Negligible);
    assert_eq!(ratings[1].category, HarmCategory::DangerousContent);
    assert_eq!(ratings[1].probability, HarmProbability::Low);
    assert_eq!(ratings[2].category, HarmCategory::Harassment);
    assert_eq!(ratings[2].probability, HarmProbability::Medium);
}

// ── Prompt blocked response ─────────────────────────────────────────

#[test]
fn parse_blocked_prompt_response() {
    let json = json!({
        "candidates": [],
        "promptFeedback": {
            "blockReason": "SAFETY",
            "safetyRatings": [
                {"category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "probability": "HIGH"}
            ]
        }
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert!(resp.candidates.is_empty());
    assert_eq!(resp.text(), ""); // graceful empty

    let feedback = resp.prompt_feedback.as_ref().unwrap();
    assert_eq!(feedback.block_reason, Some(BlockReason::Safety));
    assert_eq!(feedback.safety_ratings.len(), 1);
    assert_eq!(feedback.safety_ratings[0].probability, HarmProbability::High);
}

#[test]
fn parse_blocked_prompt_numeric_block_reason() {
    let json = json!({
        "candidates": [],
        "promptFeedback": {
            "blockReason": "MODEL_ARMOR",
            "safetyRatings": []
        }
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let feedback = resp.prompt_feedback.as_ref().unwrap();
    assert_eq!(feedback.block_reason, Some(BlockReason::ModelArmor));
}

// ── Streaming chunk (partial response) ──────────────────────────────

#[test]
fn parse_streaming_chunk_partial() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "partial "}], "role": "model"},
            "index": 0
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.text(), "partial ");
    // No finish reason on partial chunks
    assert_eq!(resp.candidates[0].finish_reason, None);
    // No usage metadata on partial chunks
    assert!(resp.usage_metadata.is_none());
}

#[test]
fn parse_streaming_final_chunk() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "done."}], "role": "model"},
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 50,
            "totalTokenCount": 60
        }
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Stop));
    assert!(resp.usage_metadata.is_some());
}

// ── Empty / minimal responses ───────────────────────────────────────

#[test]
fn parse_empty_candidates() {
    let json = json!({"candidates": []});
    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert!(resp.candidates.is_empty());
    assert_eq!(resp.text(), "");
    assert!(resp.function_calls().is_empty());
}

#[test]
fn parse_minimal_response_no_optional_fields() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "hi"}]}
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.text(), "hi");
    assert!(resp.candidates[0].safety_ratings.is_none());
    assert!(resp.candidates[0].citation_metadata.is_none());
    assert!(resp.candidates[0].grounding_metadata.is_none());
    assert!(resp.candidates[0].finish_reason.is_none());
    assert!(resp.candidates[0].index.is_none());
    assert!(resp.prompt_feedback.is_none());
    assert!(resp.usage_metadata.is_none());
    assert!(resp.model_version.is_none());
    assert!(resp.response_id.is_none());
}

// ── Function call response ──────────────────────────────────────────

#[test]
fn parse_function_call_response() {
    let json = json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "functionCall": {
                        "name": "get_weather",
                        "args": {"location": "Seattle", "unit": "celsius"}
                    }
                }],
                "role": "model"
            },
            "finishReason": "STOP"
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let calls = resp.function_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(calls[0].args["location"], "Seattle");
    assert_eq!(calls[0].args["unit"], "celsius");
}

#[test]
fn parse_multiple_function_calls() {
    let json = json!({
        "candidates": [{
            "content": {
                "parts": [
                    {"functionCall": {"name": "search", "args": {"q": "rust"}}},
                    {"functionCall": {"name": "fetch", "args": {"url": "https://example.com"}}}
                ],
                "role": "model"
            },
            "finishReason": "STOP"
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let calls = resp.function_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "search");
    assert_eq!(calls[1].name, "fetch");
}

// ── InlineData part ─────────────────────────────────────────────────

#[test]
fn parse_inline_data_response() {
    let json = json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "inlineData": {
                        "mimeType": "image/png",
                        "data": "iVBORw0KGgoAAAANSUhEUg=="
                    }
                }],
                "role": "model"
            },
            "finishReason": "STOP"
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let parts = resp.candidates[0].content.parts.as_ref().unwrap();
    match &parts[0] {
        Part::InlineData { inline_data } => {
            assert_eq!(inline_data.mime_type, "image/png");
            assert_eq!(inline_data.data, "iVBORw0KGgoAAAANSUhEUg==");
        }
        _ => panic!("Expected InlineData part"),
    }
    // text() should return empty for non-text parts
    assert_eq!(resp.text(), "");
}

// ── Mixed parts in single response ──────────────────────────────────

#[test]
fn parse_mixed_text_and_function_call() {
    let json = json!({
        "candidates": [{
            "content": {
                "parts": [
                    {"text": "Let me check the weather for you."},
                    {"functionCall": {"name": "get_weather", "args": {"city": "Tokyo"}}}
                ],
                "role": "model"
            },
            "finishReason": "STOP"
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let parts = resp.candidates[0].content.parts.as_ref().unwrap();
    assert_eq!(parts.len(), 2);

    // text() returns first text part
    assert_eq!(resp.text(), "Let me check the weather for you.");
    // function_calls() finds the function call
    assert_eq!(resp.function_calls().len(), 1);
    assert_eq!(resp.function_calls()[0].name, "get_weather");
}

// ── Grounding metadata ──────────────────────────────────────────────

#[test]
fn parse_grounding_metadata_response() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "Grounded answer"}], "role": "model"},
            "finishReason": "STOP",
            "groundingMetadata": {
                "groundingChunks": [
                    {"web": {"uri": "https://example.com/source", "title": "Source Page"}}
                ],
                "groundingSupports": [{
                    "segment": {"startIndex": 0, "endIndex": 15, "text": "Grounded answer"},
                    "groundingChunkIndices": [0]
                }],
                "webSearchQueries": ["example query"]
            }
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let grounding = resp.candidates[0].grounding_metadata.as_ref().unwrap();

    let chunks = grounding.grounding_chunks.as_ref().unwrap();
    assert_eq!(chunks.len(), 1);
    let web = chunks[0].web.as_ref().unwrap();
    assert_eq!(web.title.as_deref(), Some("Source Page"));

    let supports = grounding.grounding_supports.as_ref().unwrap();
    assert_eq!(supports[0].grounding_chunk_indices, vec![0]);
    assert_eq!(supports[0].segment.text.as_deref(), Some("Grounded answer"));

    let queries = grounding.web_search_queries.as_ref().unwrap();
    assert_eq!(queries, &["example query"]);
}

// ── Usage metadata with thinking tokens ─────────────────────────────

#[test]
fn parse_usage_metadata_with_thinking_and_prompt_details() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "answer"}], "role": "model"},
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 50,
            "totalTokenCount": 300,
            "thoughtsTokenCount": 150,
            "promptTokensDetails": [
                {"modality": "TEXT", "tokenCount": 80},
                {"modality": "IMAGE", "tokenCount": 20}
            ]
        }
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let usage = resp.usage_metadata.as_ref().unwrap();
    assert_eq!(usage.prompt_token_count, Some(100));
    assert_eq!(usage.candidates_token_count, Some(50));
    assert_eq!(usage.total_token_count, Some(300));
    assert_eq!(usage.thoughts_token_count, Some(150));

    let details = usage.prompt_tokens_details.as_ref().unwrap();
    assert_eq!(details.len(), 2);
    assert_eq!(details[0].modality, Modality::Text);
    assert_eq!(details[0].token_count, 80);
    assert_eq!(details[1].modality, Modality::Image);
    assert_eq!(details[1].token_count, 20);
}

// ── Vertex numeric prompt token details ─────────────────────────────

#[test]
fn parse_vertex_numeric_modality_in_prompt_details() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "ok"}], "role": "model"},
            "finishReason": 1
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 2,
            "totalTokenCount": 12,
            "promptTokensDetails": [
                {"modality": 1, "tokenCount": 10}
            ]
        }
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let details = resp.usage_metadata.as_ref().unwrap().prompt_tokens_details.as_ref().unwrap();
    assert_eq!(details[0].modality, Modality::Text);
}

// ── All FinishReason variants ───────────────────────────────────────

#[test]
fn parse_all_finish_reason_strings() {
    for (s, expected) in [
        ("STOP", FinishReason::Stop),
        ("MAX_TOKENS", FinishReason::MaxTokens),
        ("SAFETY", FinishReason::Safety),
        ("RECITATION", FinishReason::Recitation),
        ("OTHER", FinishReason::Other),
        ("BLOCKLIST", FinishReason::Blocklist),
        ("PROHIBITED_CONTENT", FinishReason::ProhibitedContent),
        ("SPII", FinishReason::Spii),
        ("MALFORMED_FUNCTION_CALL", FinishReason::MalformedFunctionCall),
    ] {
        let json = json!({
            "candidates": [{
                "content": {"parts": [{"text": "x"}]},
                "finishReason": s
            }]
        });
        let resp: GenerationResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.candidates[0].finish_reason, Some(expected), "failed for {s}");
    }
}

// ── All FinishReason numeric variants (Vertex) ──────────────────────

#[test]
fn parse_all_finish_reason_numbers() {
    for (n, expected) in [
        (1, FinishReason::Stop),
        (2, FinishReason::MaxTokens),
        (3, FinishReason::Safety),
        (4, FinishReason::Recitation),
        (5, FinishReason::Other),
        (6, FinishReason::Blocklist),
        (7, FinishReason::ProhibitedContent),
        (8, FinishReason::Spii),
        (9, FinishReason::MalformedFunctionCall),
    ] {
        let json = json!({
            "candidates": [{
                "content": {"parts": [{"text": "x"}]},
                "finishReason": n
            }]
        });
        let resp: GenerationResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.candidates[0].finish_reason, Some(expected), "failed for {n}");
    }
}

// ── Citation metadata ───────────────────────────────────────────────

#[test]
fn parse_citation_metadata() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "cited text"}], "role": "model"},
            "finishReason": "STOP",
            "citationMetadata": {
                "citationSources": [{
                    "uri": "https://example.com/article",
                    "title": "Example Article",
                    "startIndex": 0,
                    "endIndex": 10,
                    "license": "CC-BY-4.0"
                }]
            }
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let citations = &resp.candidates[0].citation_metadata.as_ref().unwrap().citation_sources;
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].uri.as_deref(), Some("https://example.com/article"));
    assert_eq!(citations[0].title.as_deref(), Some("Example Article"));
    assert_eq!(citations[0].start_index, Some(0));
    assert_eq!(citations[0].end_index, Some(10));
    assert_eq!(citations[0].license.as_deref(), Some("CC-BY-4.0"));
}

#[test]
fn parse_citation_metadata_without_citation_sources() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "grounded text"}], "role": "model"},
            "finishReason": "STOP",
            "citationMetadata": {}
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let meta = resp.candidates[0].citation_metadata.as_ref().unwrap();
    assert!(meta.citation_sources.is_empty());
}

#[test]
fn parse_citation_metadata_with_populated_citation_sources() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "cited text"}], "role": "model"},
            "finishReason": "STOP",
            "citationMetadata": {
                "citationSources": [
                    {
                        "uri": "https://example.com/a",
                        "title": "Article A",
                        "startIndex": 0,
                        "endIndex": 5
                    },
                    {
                        "uri": "https://example.com/b",
                        "startIndex": 6,
                        "endIndex": 12
                    }
                ]
            }
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let citations = &resp.candidates[0].citation_metadata.as_ref().unwrap().citation_sources;
    assert_eq!(citations.len(), 2);
    assert_eq!(citations[0].uri.as_deref(), Some("https://example.com/a"));
    assert_eq!(citations[0].title.as_deref(), Some("Article A"));
    assert_eq!(citations[1].uri.as_deref(), Some("https://example.com/b"));
    assert_eq!(citations[1].title, None);
}

// ── Round-trip serialization ────────────────────────────────────────

#[test]
fn roundtrip_generation_response() {
    let json = json!({
        "candidates": [{
            "content": {
                "parts": [{"text": "round trip test"}],
                "role": "model"
            },
            "finishReason": "STOP",
            "safetyRatings": [
                {"category": "HARM_CATEGORY_HATE_SPEECH", "probability": "NEGLIGIBLE"}
            ],
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": 5,
            "candidatesTokenCount": 3,
            "totalTokenCount": 8
        },
        "modelVersion": "gemini-2.5-flash",
        "responseId": "rt-123"
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    let serialized = serde_json::to_string(&resp).unwrap();
    let resp2: GenerationResponse = serde_json::from_str(&serialized).unwrap();
    assert_eq!(resp, resp2);
}

// ── Unknown / future enum values degrade gracefully ─────────────────

#[test]
fn parse_unknown_finish_reason_string() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "x"}]},
            "finishReason": "SOME_FUTURE_REASON"
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Other));
}

#[test]
fn parse_unknown_finish_reason_number() {
    let json = json!({
        "candidates": [{
            "content": {"parts": [{"text": "x"}]},
            "finishReason": 999
        }]
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Other));
}

#[test]
fn parse_unknown_harm_category_degrades() {
    let rating: SafetyRating = serde_json::from_value(json!({
        "category": "HARM_CATEGORY_FUTURE_THING",
        "probability": "NEGLIGIBLE"
    }))
    .unwrap();
    assert_eq!(rating.category, HarmCategory::Unspecified);
}

// ── Vertex full response with all numeric enums ─────────────────────

#[test]
fn parse_full_vertex_response_numeric_enums() {
    let json = json!({
        "candidates": [{
            "content": {
                "parts": [{"text": "Vertex says hello"}],
                "role": "model"
            },
            "finishReason": 1,
            "safetyRatings": [
                {"category": 1, "probability": 1},
                {"category": 2, "probability": 1},
                {"category": 3, "probability": 1},
                {"category": 4, "probability": 1}
            ],
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": 20,
            "candidatesTokenCount": 10,
            "totalTokenCount": 30,
            "promptTokensDetails": [
                {"modality": 1, "tokenCount": 20}
            ]
        }
    });

    let resp: GenerationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.text(), "Vertex says hello");
    assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Stop));

    let ratings = resp.candidates[0].safety_ratings.as_ref().unwrap();
    assert_eq!(ratings.len(), 4);
    // All should parse without error
    for r in ratings {
        assert_ne!(r.category, HarmCategory::Unspecified);
        assert_eq!(r.probability, HarmProbability::Negligible);
    }
}
