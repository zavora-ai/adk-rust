//! README Agent Evaluation snippet validation

use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator};

fn main() {
    // Validate the README snippet compiles
    let config = EvaluationConfig::with_criteria(
        EvaluationCriteria::exact_tools().with_response_similarity(0.8),
    );

    let _evaluator = Evaluator::new(config);

    // Note: evaluate_file requires an agent and async context
    // This validates the types and methods exist

    println!("✓ Eval snippet compiles");
}
