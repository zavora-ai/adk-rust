//! Property P4: Sentence Chunker Completeness
//!
//! *For any* sequence of tokens pushed to a `SentenceChunker`, the concatenation
//! of all returned sentences plus the final `flush()` output SHALL equal the
//! concatenation of all input tokens (modulo whitespace trimming).
//!
//! **Validates: Requirement 8**

use adk_audio::SentenceChunker;
use proptest::prelude::*;

fn arb_token() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-zA-Z]{1,10}",
        Just(" ".to_string()),
        Just(". ".to_string()),
        Just("! ".to_string()),
        Just("? ".to_string()),
        Just(";\n".to_string()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// P4: All input content is preserved across push + flush
    #[test]
    fn prop_chunker_completeness(tokens in proptest::collection::vec(arb_token(), 1..20)) {
        let mut chunker = SentenceChunker::new();
        let input_concat: String = tokens.iter().cloned().collect();

        let mut output_parts: Vec<String> = Vec::new();
        for token in &tokens {
            let sentences = chunker.push(token);
            output_parts.extend(sentences);
        }
        if let Some(remaining) = chunker.flush() {
            output_parts.push(remaining);
        }

        // Normalize: remove whitespace for comparison
        let input_normalized: String = input_concat.chars().filter(|c| !c.is_whitespace()).collect();
        let output_normalized: String = output_parts.join("").chars().filter(|c| !c.is_whitespace()).collect();

        prop_assert_eq!(
            &output_normalized,
            &input_normalized,
            "content mismatch: input={:?}, output={:?}", input_normalized, output_normalized
        );
    }

    /// P4.2: Empty input produces no output
    #[test]
    fn prop_empty_input(_dummy in 0..1u8) {
        let mut chunker = SentenceChunker::new();
        let sentences = chunker.push("");
        prop_assert!(sentences.is_empty());
        prop_assert!(chunker.flush().is_none());
    }
}
