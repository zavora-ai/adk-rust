//! The prompt optimizer rewrites the instruction and evaluates the agent built from each
//! proposal, so a round changes the prompt when the proposal scores better.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::Agent;
use adk_eval::optimizer::{
    AgentCandidates, InstructionCandidates, OptimizerConfig, PromptOptimizer,
};
use adk_eval::schema::ContentData;
use adk_eval::{EvalCase, EvalSet, EvaluationConfig, EvaluationCriteria, Evaluator, Turn};
use adk_model::{ScriptedLlm, ScriptedResponse};

const IMPROVED: &str = "Answer with the capital city only.";

fn eval_set() -> EvalSet {
    EvalSet {
        eval_set_id: "capitals".to_string(),
        name: "capitals".to_string(),
        description: "one capital".to_string(),
        test_files: vec![],
        eval_cases: vec![EvalCase {
            eval_id: "france".to_string(),
            description: "France".to_string(),
            conversation: vec![Turn {
                invocation_id: "inv-1".to_string(),
                user_content: ContentData::text("What is the capital of France?"),
                final_response: Some(ContentData::model_response("Paris")),
                intermediate_data: None,
            }],
            session_input: Default::default(),
            tags: vec![],
            metadata: None,
        }],
    }
}

/// An agent whose model answers correctly only under an instruction that names the task.
fn candidates(seen: Arc<std::sync::Mutex<Vec<String>>>) -> InstructionCandidates {
    InstructionCandidates::new("Answer questions", move |instruction| {
        seen.lock().unwrap().push(instruction.to_string());
        let reply = if instruction.contains("capital") { "Paris" } else { "I am not sure." };
        let model = Arc::new(ScriptedLlm::new("agent", vec![ScriptedResponse::text(reply)]));
        let agent: Arc<dyn Agent> =
            Arc::new(LlmAgentBuilder::new("tutor").model(model).instruction(instruction).build()?);
        Ok(agent)
    })
}

fn evaluator() -> Evaluator {
    Evaluator::new(EvaluationConfig {
        criteria: EvaluationCriteria { response_similarity: Some(0.9), ..Default::default() },
        continue_on_failure: true,
        ..Default::default()
    })
}

#[tokio::test]
async fn one_round_changes_the_prompt_and_evaluates_the_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("optimized.txt");
    let optimizer_llm =
        Arc::new(ScriptedLlm::new("optimizer", vec![ScriptedResponse::text(IMPROVED)]));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let optimizer = PromptOptimizer::new(
        optimizer_llm.clone(),
        evaluator(),
        OptimizerConfig { max_iterations: 1, target_threshold: 0.9, output_path: output.clone() },
    );

    let result =
        optimizer.optimize_candidates(&candidates(seen.clone()), &eval_set()).await.unwrap();

    assert_eq!(result.best_instructions, IMPROVED, "the prompt changed after one round");
    assert_eq!(result.iterations_run, 1);
    assert!(result.initial_score < result.final_score, "{result:?}");
    assert!(result.final_score >= 0.9, "{result:?}");
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["Answer questions".to_string(), IMPROVED.to_string()],
        "the baseline and the candidate were each built and evaluated"
    );
    assert_eq!(std::fs::read_to_string(&output).unwrap(), IMPROVED);
    // The optimizer was asked about the instruction, not the description.
    let prompt = serde_json::to_string(&optimizer_llm.requests()[0].contents).unwrap();
    assert!(prompt.contains("Answer questions"), "{prompt}");
}

#[tokio::test]
async fn an_opaque_agent_below_the_threshold_is_refused_with_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let optimizer_llm =
        Arc::new(ScriptedLlm::new("optimizer", vec![ScriptedResponse::text(IMPROVED)]));
    let optimizer = PromptOptimizer::new(
        optimizer_llm,
        evaluator(),
        OptimizerConfig {
            max_iterations: 1,
            target_threshold: 0.9,
            output_path: dir.path().join("optimized.txt"),
        },
    );
    let agent = candidates(Arc::default()).build("Answer questions").unwrap();

    let error = optimizer.optimize(agent, &eval_set()).await.unwrap_err().to_string();
    assert!(error.contains("optimize_candidates"), "{error}");
}
