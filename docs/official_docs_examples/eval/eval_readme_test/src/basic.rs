//! Validates adk-eval README examples compile correctly

use adk_eval::{
    EvaluationConfig, EvaluationCriteria, Evaluator,
    Rubric, ToolTrajectoryConfig, ResponseMatchConfig,
};

// Validate: Basic evaluator creation
fn _basic_example() {
    let config = EvaluationConfig::with_criteria(
        EvaluationCriteria::exact_tools()
            .with_response_similarity(0.8)
    );
    let _evaluator = Evaluator::new(config);
}

// Validate: Tool trajectory config
fn _tool_trajectory_example() {
    let _criteria = EvaluationCriteria {
        tool_trajectory_score: Some(1.0),
        tool_trajectory_config: Some(ToolTrajectoryConfig {
            strict_order: true,
            strict_args: false,
        }),
        ..Default::default()
    };
}

// Validate: Response similarity config
fn _response_similarity_example() {
    use adk_eval::criteria::SimilarityAlgorithm;
    
    let _criteria = EvaluationCriteria {
        response_similarity: Some(0.8),
        response_match_config: Some(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Jaccard,
            ignore_case: true,
            normalize: true,
            ..Default::default()
        }),
        ..Default::default()
    };
}

// Validate: Semantic match
fn _semantic_match_example() {
    let _criteria = EvaluationCriteria::semantic_match(0.85);
}

// Validate: Rubric-based evaluation
fn _rubric_example() {
    let _criteria = EvaluationCriteria::default()
        .with_rubrics(0.7, vec![
            Rubric::new("Accuracy", "Response is factually correct")
                .with_weight(0.5),
            Rubric::new("Helpfulness", "Response addresses user's needs")
                .with_weight(0.3),
            Rubric::new("Clarity", "Response is clear and well-organized")
                .with_weight(0.2),
        ]);
}

// Validate: Safety and hallucination
fn _safety_example() {
    let _criteria = EvaluationCriteria {
        safety_score: Some(0.95),
        hallucination_score: Some(0.9),
        ..Default::default()
    };
}

fn main() {
    println!("✓ EvaluationConfig::with_criteria() compiles");
    println!("✓ EvaluationCriteria::exact_tools() compiles");
    println!("✓ .with_response_similarity() compiles");
    println!("✓ Evaluator::new() compiles");
    println!("✓ ToolTrajectoryConfig compiles");
    println!("✓ ResponseMatchConfig compiles");
    println!("✓ SimilarityAlgorithm::Jaccard compiles");
    println!("✓ EvaluationCriteria::semantic_match() compiles");
    println!("✓ .with_rubrics() compiles");
    println!("✓ Rubric::new().with_weight() compiles");
    println!("✓ safety_score/hallucination_score fields compile");
    println!("\nadk-eval README validation passed!");
}
