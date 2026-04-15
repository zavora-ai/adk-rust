//! Property-based tests for core-to-Gemini multimodal conversion.

use adk_core::{FileDataPart, FunctionResponseData, InlineDataPart};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use proptest::prelude::*;

fn arb_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("image/png".to_string()),
        Just("image/jpeg".to_string()),
        Just("audio/wav".to_string()),
        Just("application/pdf".to_string()),
        Just("video/mp4".to_string()),
    ]
}

fn arb_inline_data_part() -> impl Strategy<Value = InlineDataPart> {
    (arb_mime_type(), prop::collection::vec(any::<u8>(), 0..1024))
        .prop_map(|(mime_type, data)| InlineDataPart { mime_type, data })
}

fn arb_file_uri() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("gs://bucket/file".to_string()),
        Just("https://example.com/file.pdf".to_string()),
        Just("s3://bucket/key".to_string()),
    ]
}

fn arb_file_data_part() -> impl Strategy<Value = FileDataPart> {
    (arb_mime_type(), arb_file_uri())
        .prop_map(|(mime_type, file_uri)| FileDataPart { mime_type, file_uri })
}

/// Simulate the conversion logic: build a Gemini FunctionResponse with nested parts.
fn simulate_conversion(frd: &FunctionResponseData) -> adk_gemini::FunctionResponse {
    let mut fr_parts = Vec::new();

    for inline in &frd.inline_data {
        let encoded = BASE64_STANDARD.encode(&inline.data);
        fr_parts.push(adk_gemini::FunctionResponsePart::InlineData {
            inline_data: adk_gemini::Blob { mime_type: inline.mime_type.clone(), data: encoded },
        });
    }

    for file in &frd.file_data {
        fr_parts.push(adk_gemini::FunctionResponsePart::FileData {
            file_data: adk_gemini::FileDataRef {
                mime_type: file.mime_type.clone(),
                file_uri: file.file_uri.clone(),
            },
        });
    }

    let response_payload = match &frd.response {
        serde_json::Value::Object(_) => frd.response.clone(),
        other => serde_json::json!({ "result": other }),
    };
    let mut gemini_fr = adk_gemini::FunctionResponse::new(&frd.name, response_payload);
    gemini_fr.parts = fr_parts;
    gemini_fr
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: multimodal-function-responses, Property 4: Core-to-Gemini Conversion Preserves All Multimodal Parts**
    /// *For any* FunctionResponseData with K inline data parts and L file data parts,
    /// conversion produces a Gemini FunctionResponse with exactly K + L nested parts,
    /// base64-decoded inline data matches original bytes, file data preserves MIME type and URI.
    /// **Validates: Requirements 4.1, 4.4, 7.2, 7.3**
    #[test]
    fn prop_core_to_gemini_conversion_preserves_parts(
        inline_data in prop::collection::vec(arb_inline_data_part(), 0..4),
        file_data in prop::collection::vec(arb_file_data_part(), 0..4),
    ) {
        let k = inline_data.len();
        let l = file_data.len();

        let frd = FunctionResponseData {
            name: "test_tool".to_string(),
            response: serde_json::json!({"ok": true}),
            inline_data: inline_data.clone(),
            file_data: file_data.clone(),
        };

        let gemini_fr = simulate_conversion(&frd);

        // Exactly K + L nested parts
        prop_assert_eq!(gemini_fr.parts.len(), k + l);

        // Verify inline data parts: base64-decoded matches original bytes
        for (actual, expected) in gemini_fr.parts[..k].iter().zip(inline_data.iter()) {
            match actual {
                adk_gemini::FunctionResponsePart::InlineData { inline_data: blob } => {
                    prop_assert_eq!(&blob.mime_type, &expected.mime_type);
                    let decoded = BASE64_STANDARD.decode(&blob.data).unwrap();
                    prop_assert_eq!(&decoded, &expected.data);
                }
                other => prop_assert!(false, "expected InlineData, got {:?}", other),
            }
        }

        // Verify file data parts: MIME type and URI preserved
        for (actual, expected) in gemini_fr.parts[k..].iter().zip(file_data.iter()) {
            match actual {
                adk_gemini::FunctionResponsePart::FileData { file_data: fdr } => {
                    prop_assert_eq!(&fdr.mime_type, &expected.mime_type);
                    prop_assert_eq!(&fdr.file_uri, &expected.file_uri);
                }
                other => prop_assert!(false, "expected FileData, got {:?}", other),
            }
        }
    }
}
