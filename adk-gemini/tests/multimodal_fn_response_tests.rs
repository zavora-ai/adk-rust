//! Property-based tests for multimodal Content assembly in adk-gemini.

use adk_gemini::{Blob, Content, FileDataRef, FunctionResponsePart, Part};
use proptest::prelude::*;

fn arb_blob() -> impl Strategy<Value = Blob> {
    (
        prop_oneof![
            Just("image/png".to_string()),
            Just("image/jpeg".to_string()),
            Just("audio/wav".to_string()),
        ],
        "[a-zA-Z0-9+/=]{4,64}",
    )
        .prop_map(|(mime_type, data)| Blob::new(mime_type, data))
}

fn arb_file_data_ref() -> impl Strategy<Value = FileDataRef> {
    (
        prop_oneof![Just("application/pdf".to_string()), Just("video/mp4".to_string()),],
        prop_oneof![
            Just("gs://bucket/file".to_string()),
            Just("https://example.com/file.pdf".to_string()),
        ],
    )
        .prop_map(|(mime_type, file_uri)| FileDataRef { mime_type, file_uri })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: multimodal-function-responses, Property 3: Multimodal Content Assembly**
    /// *For any* FunctionResponse with N inline data blobs and M file data references,
    /// Content::function_response_multimodal() produces a single FunctionResponse Part
    /// whose nested `parts` field has exactly N + M entries in correct order.
    /// **Validates: Requirements 2.1, 2.3, 3.3**
    #[test]
    fn prop_multimodal_content_assembly_nested(
        inline_data in prop::collection::vec(arb_blob(), 0..5),
        file_data in prop::collection::vec(arb_file_data_ref(), 0..5),
    ) {
        let n = inline_data.len();
        let m = file_data.len();

        let mut fr_parts: Vec<FunctionResponsePart> = inline_data
            .iter()
            .map(|b| FunctionResponsePart::InlineData { inline_data: b.clone() })
            .collect();
        fr_parts.extend(
            file_data
                .iter()
                .map(|f| FunctionResponsePart::FileData { file_data: f.clone() }),
        );

        let fr = adk_gemini::FunctionResponse {
            name: "test_tool".to_string(),
            response: Some(serde_json::json!({"ok": true})),
            parts: fr_parts,
        };

        let content = Content::function_response_multimodal(fr);
        let content_parts = content.parts.as_ref().expect("parts should be Some");

        // Single FunctionResponse part in the Content
        prop_assert_eq!(content_parts.len(), 1);
        let is_fn_response = matches!(&content_parts[0], Part::FunctionResponse { .. });
        prop_assert!(is_fn_response, "should be a FunctionResponse part");

        // Nested parts inside the FunctionResponse
        if let Part::FunctionResponse { function_response, .. } = &content_parts[0] {
            prop_assert_eq!(function_response.parts.len(), n + m);

            // First N are InlineData (order preserved)
            for (actual, expected) in function_response.parts[..n].iter().zip(inline_data.iter()) {
                match actual {
                    FunctionResponsePart::InlineData { inline_data: blob } => {
                        prop_assert_eq!(&blob.mime_type, &expected.mime_type);
                        prop_assert_eq!(&blob.data, &expected.data);
                    }
                    other => prop_assert!(false, "expected InlineData, got {:?}", other),
                }
            }

            // Next M are FileData (order preserved)
            for (actual, expected) in function_response.parts[n..].iter().zip(file_data.iter()) {
                match actual {
                    FunctionResponsePart::FileData { file_data: fdr } => {
                        prop_assert_eq!(&fdr.mime_type, &expected.mime_type);
                        prop_assert_eq!(&fdr.file_uri, &expected.file_uri);
                    }
                    other => prop_assert!(false, "expected FileData, got {:?}", other),
                }
            }
        }
    }
}
