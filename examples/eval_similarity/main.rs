//! Response Similarity Example
//!
//! This example demonstrates the various text similarity algorithms
//! available for comparing expected vs actual agent responses.
//!
//! Run with: cargo run --example eval_similarity

use adk_eval::criteria::SimilarityAlgorithm;
use adk_eval::{EvaluationCriteria, ResponseMatchConfig, ResponseScorer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Response Similarity Scoring ===\n");

    // -------------------------------------------------------------------------
    // 1. Default Jaccard similarity (word overlap)
    // -------------------------------------------------------------------------
    println!("1. Jaccard Similarity (default - word overlap)...\n");

    let scorer = ResponseScorer::new();

    let expected = "The weather in New York is sunny and warm today.";
    let actual = "Today's weather in New York is warm and sunny.";

    let score = scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("Jaccard Score: {:.1}% (word overlap)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 2. Exact match
    // -------------------------------------------------------------------------
    println!("2. Exact Match...\n");

    let exact_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Exact,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: false,
    });

    // Case difference with normalization
    let expected = "Hello World";
    let actual = "hello world";
    let score = exact_scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("Exact (ignore_case=true): {:.1}%\n", score * 100.0);

    // Different text
    let actual = "Hello there";
    let score = exact_scorer.score(expected, actual);
    println!("Actual:   \"{}\"", actual);
    println!("Exact: {:.1}% (different text = 0)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 3. Contains check
    // -------------------------------------------------------------------------
    println!("3. Contains Check...\n");

    let contains_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Contains,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: false,
    });

    let expected = "error";
    let actual = "An error occurred while processing your request.";

    let score = contains_scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("Contains: {:.1}% (keyword found = 100%)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 4. Levenshtein distance (edit distance)
    // -------------------------------------------------------------------------
    println!("4. Levenshtein Distance (edit distance)...\n");

    let levenshtein_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Levenshtein,
        normalize: false,
        ignore_case: false,
        ignore_punctuation: false,
    });

    // Similar strings
    let expected = "hello";
    let actual = "hallo";
    let score = levenshtein_scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("Levenshtein: {:.1}% (1 character different)\n", score * 100.0);

    // Very different strings
    let expected = "abc";
    let actual = "xyz";
    let score = levenshtein_scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("Levenshtein: {:.1}% (completely different)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 5. ROUGE-1 (unigram overlap)
    // -------------------------------------------------------------------------
    println!("5. ROUGE-1 (unigram overlap)...\n");

    let rouge1_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Rouge1,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: false,
    });

    let expected = "The cat sat on the mat near the window.";
    let actual = "A cat was sitting on the mat by the door.";

    let score = rouge1_scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("ROUGE-1: {:.1}% (unigram recall)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 6. ROUGE-2 (bigram overlap)
    // -------------------------------------------------------------------------
    println!("6. ROUGE-2 (bigram overlap)...\n");

    let rouge2_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Rouge2,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: false,
    });

    let expected = "machine learning is fascinating";
    let actual = "machine learning is very interesting";

    let score = rouge2_scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("ROUGE-2: {:.1}% (bigram recall)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 7. ROUGE-L (longest common subsequence)
    // -------------------------------------------------------------------------
    println!("7. ROUGE-L (longest common subsequence)...\n");

    let rougel_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::RougeL,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: false,
    });

    let expected = "The quick brown fox jumps over the lazy dog.";
    let actual = "The brown fox quickly jumps over a lazy sleeping dog.";

    let score = rougel_scorer.score(expected, actual);
    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("ROUGE-L: {:.1}% (LCS F1 score)\n", score * 100.0);

    // -------------------------------------------------------------------------
    // 8. Comparing all algorithms on the same text
    // -------------------------------------------------------------------------
    println!("8. Algorithm comparison on same texts...\n");

    let expected = "The answer is 42.";
    let actual = "42 is the answer.";

    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!();

    let algorithms = [
        ("Exact", SimilarityAlgorithm::Exact),
        ("Contains", SimilarityAlgorithm::Contains),
        ("Levenshtein", SimilarityAlgorithm::Levenshtein),
        ("Jaccard", SimilarityAlgorithm::Jaccard),
        ("ROUGE-1", SimilarityAlgorithm::Rouge1),
        ("ROUGE-2", SimilarityAlgorithm::Rouge2),
        ("ROUGE-L", SimilarityAlgorithm::RougeL),
    ];

    for (name, algo) in algorithms {
        let scorer = ResponseScorer::with_config(ResponseMatchConfig {
            algorithm: algo,
            normalize: true,
            ignore_case: true,
            ignore_punctuation: false,
        });
        let score = scorer.score(expected, actual);
        println!("  {:12}: {:5.1}%", name, score * 100.0);
    }

    // -------------------------------------------------------------------------
    // 9. Normalization effects
    // -------------------------------------------------------------------------
    println!("\n9. Normalization effects...\n");

    let expected = "Hello, World!";
    let actual = "hello world";

    // Without normalization
    let strict_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Exact,
        normalize: false,
        ignore_case: false,
        ignore_punctuation: false,
    });

    // With full normalization
    let normalized_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Exact,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: true,
    });

    println!("Expected: \"{}\"", expected);
    println!("Actual:   \"{}\"", actual);
    println!("  Strict (no normalization): {:.1}%", strict_scorer.score(expected, actual) * 100.0);
    println!(
        "  Normalized (ignore case/punct): {:.1}%\n",
        normalized_scorer.score(expected, actual) * 100.0
    );

    // -------------------------------------------------------------------------
    // 10. Using with EvaluationCriteria
    // -------------------------------------------------------------------------
    println!("10. Configuring criteria for evaluation...\n");

    let _criteria = EvaluationCriteria {
        response_similarity: Some(0.8), // Require 80% similarity
        response_match_config: Some(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Jaccard,
            normalize: true,
            ignore_case: true,
            ignore_punctuation: false,
        }),
        ..Default::default()
    };

    println!("Configured criteria:");
    println!("  - Threshold: 80%");
    println!("  - Algorithm: Jaccard (word overlap)");
    println!("  - Normalize: true");
    println!("  - Ignore case: true");

    // Shorthand
    let criteria = EvaluationCriteria::response_similarity(0.8);
    println!("\nShorthand: EvaluationCriteria::response_similarity(0.8)");
    println!("  - Threshold: {:?}", criteria.response_similarity);

    println!("\n=== Example Complete ===");
    println!("\nAlgorithm Selection Guide:");
    println!("  - Exact:       When responses must match exactly");
    println!("  - Contains:    When checking for specific keywords");
    println!("  - Levenshtein: For typo tolerance / fuzzy matching");
    println!("  - Jaccard:     For word overlap (default, good general use)");
    println!("  - ROUGE-1:     For summarization tasks (unigram recall)");
    println!("  - ROUGE-2:     For phrase-level matching");
    println!("  - ROUGE-L:     For preserving word order (LCS)");

    Ok(())
}
