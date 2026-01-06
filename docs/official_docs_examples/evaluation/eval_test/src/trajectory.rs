//! Trajectory evaluation doc-test - validates tool trajectory scoring
//! from evaluation.md documentation

use adk_eval::{
    EvaluationCriteria, ResponseMatchConfig, ToolTrajectoryConfig,
    ToolTrajectoryScorer, ToolUse, Rubric,
};
use adk_eval::criteria::SimilarityAlgorithm;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Evaluation Doc-Test: Trajectory ===\n");

    // Tool trajectory matching from docs
    let criteria = EvaluationCriteria {
        tool_trajectory_score: Some(1.0),
        tool_trajectory_config: Some(ToolTrajectoryConfig {
            strict_order: true,
            strict_args: false,
        }),
        ..Default::default()
    };
    println!("✓ Created EvaluationCriteria with tool trajectory");
    assert_eq!(criteria.tool_trajectory_score, Some(1.0));

    // Response similarity from docs
    let criteria_with_response = EvaluationCriteria {
        response_similarity: Some(0.8),
        response_match_config: Some(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Jaccard,
            ignore_case: true,
            normalize: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    println!("✓ Created criteria with response similarity");
    assert_eq!(criteria_with_response.response_similarity, Some(0.8));

    // Builder pattern from docs
    let builder_criteria = EvaluationCriteria::exact_tools()
        .with_response_similarity(0.8);
    println!("✓ Builder pattern works");
    assert_eq!(builder_criteria.tool_trajectory_score, Some(1.0));
    assert_eq!(builder_criteria.response_similarity, Some(0.8));

    // Semantic match from docs
    let semantic_criteria = EvaluationCriteria::semantic_match(0.85);
    println!("✓ Semantic match criteria");
    assert_eq!(semantic_criteria.semantic_match_score, Some(0.85));

    // Rubric-based evaluation from docs
    let rubric_criteria = EvaluationCriteria::default()
        .with_rubrics(0.7, vec![
            Rubric::new("Accuracy", "Response is factually correct")
                .with_weight(0.5),
            Rubric::new("Helpfulness", "Response addresses user's needs")
                .with_weight(0.3),
            Rubric::new("Clarity", "Response is clear and well-organized")
                .with_weight(0.2),
        ]);
    println!("✓ Rubric-based criteria");
    assert_eq!(rubric_criteria.rubric_quality_score, Some(0.7));
    assert_eq!(rubric_criteria.rubric_config.as_ref().unwrap().rubrics.len(), 3);

    // Tool trajectory scorer
    let scorer = ToolTrajectoryScorer::with_config(ToolTrajectoryConfig {
        strict_order: true,
        strict_args: false,
    });

    let expected = vec![
        ToolUse::new("get_weather").with_args(json!({"location": "NYC"})),
    ];
    let actual = vec![
        ToolUse::new("get_weather").with_args(json!({"location": "NYC", "units": "fahrenheit"})),
    ];

    let score = scorer.score(&expected, &actual);
    println!("✓ Tool trajectory scoring: {:.0}%", score * 100.0);
    assert!(score > 0.9); // Should match since strict_args is false

    // Detailed comparison
    let comparison = scorer.compare(&expected, &actual);
    println!("✓ Detailed comparison: {} matched, {} missing, {} extra",
        comparison.matched.len(),
        comparison.missing.len(),
        comparison.extra.len()
    );

    // Safety and hallucination from docs
    let safety_criteria = EvaluationCriteria {
        safety_score: Some(0.95),
        hallucination_score: Some(0.9),
        ..Default::default()
    };
    println!("✓ Safety/hallucination criteria");
    assert!(safety_criteria.has_criteria());

    println!("\n=== All trajectory tests passed! ===");
    Ok(())
}
