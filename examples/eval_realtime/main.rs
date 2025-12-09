//! Realtime Agent Evaluation Example
//!
//! This example demonstrates evaluation strategies for real-time voice agents.
//! Since realtime agents involve audio I/O, evaluation focuses on:
//! - Transcript quality (speech-to-text accuracy)
//! - Response relevance and correctness
//! - Tool usage during voice sessions
//! - Turn-taking behavior
//!
//! Run with: cargo run --example eval_realtime
//!
//! Note: This example uses simulated transcripts for demonstration.
//! Real evaluation would integrate with actual audio streams.

use adk_core::Llm;
use adk_eval::criteria::{ResponseMatchConfig, RubricLevel, SimilarityAlgorithm};
use adk_eval::schema::ToolUse;
use adk_eval::{
    EvaluationConfig, EvaluationCriteria, Evaluator, LlmJudge, LlmJudgeConfig, ResponseScorer,
    Rubric, RubricConfig, ToolTrajectoryScorer,
};
use adk_model::GeminiModel;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Simulated realtime conversation turn
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RealtimeTurn {
    /// Speaker (user or assistant)
    speaker: String,
    /// Transcribed text
    transcript: String,
    /// Audio duration in seconds
    duration_secs: f64,
    /// Tool calls made during this turn
    tool_calls: Vec<String>,
    /// Latency from end of user speech to start of response
    response_latency_ms: Option<u64>,
}

/// Simulated realtime conversation for evaluation
#[derive(Debug, Clone)]
struct RealtimeConversation {
    turns: Vec<RealtimeTurn>,
    total_duration: Duration,
}

impl RealtimeConversation {
    fn new() -> Self {
        Self { turns: Vec::new(), total_duration: Duration::ZERO }
    }

    fn add_turn(&mut self, turn: RealtimeTurn) {
        self.total_duration += Duration::from_secs_f64(turn.duration_secs);
        self.turns.push(turn);
    }

    #[allow(dead_code)]
    fn get_transcript(&self) -> String {
        self.turns
            .iter()
            .map(|t| format!("{}: {}", t.speaker, t.transcript))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn get_tool_trajectory(&self) -> Vec<String> {
        self.turns.iter().flat_map(|t| t.tool_calls.clone()).collect()
    }

    fn get_assistant_responses(&self) -> Vec<String> {
        self.turns
            .iter()
            .filter(|t| t.speaker == "assistant")
            .map(|t| t.transcript.clone())
            .collect()
    }

    fn average_latency(&self) -> Option<u64> {
        let latencies: Vec<u64> = self.turns.iter().filter_map(|t| t.response_latency_ms).collect();

        if latencies.is_empty() {
            None
        } else {
            Some(latencies.iter().sum::<u64>() / latencies.len() as u64)
        }
    }
}

/// Evaluation criteria specific to realtime agents
#[derive(Debug, Clone)]
struct RealtimeEvalCriteria {
    /// Maximum acceptable response latency in ms
    max_latency_ms: u64,
    /// Minimum transcript similarity threshold
    transcript_similarity: f64,
    /// Required tool call accuracy
    tool_accuracy: f64,
}

impl Default for RealtimeEvalCriteria {
    fn default() -> Self {
        Self {
            max_latency_ms: 500,        // 500ms max latency for natural conversation
            transcript_similarity: 0.8, // 80% transcript accuracy
            tool_accuracy: 1.0,         // 100% tool call accuracy
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK-Eval: Realtime Agent Evaluation ===\n");

    // -------------------------------------------------------------------------
    // 1. Create simulated realtime conversations for evaluation
    // -------------------------------------------------------------------------
    println!("1. Creating simulated realtime conversations...\n");

    // Simulated conversation 1: Weather query with tool call
    let mut conv1 = RealtimeConversation::new();
    conv1.add_turn(RealtimeTurn {
        speaker: "user".to_string(),
        transcript: "What's the weather like in San Francisco today?".to_string(),
        duration_secs: 2.5,
        tool_calls: vec![],
        response_latency_ms: None,
    });
    conv1.add_turn(RealtimeTurn {
        speaker: "assistant".to_string(),
        transcript: "Let me check the weather for you. The weather in San Francisco is currently 65 degrees and partly cloudy.".to_string(),
        duration_secs: 4.0,
        tool_calls: vec!["get_weather".to_string()],
        response_latency_ms: Some(320),
    });

    // Simulated conversation 2: Multi-turn with follow-up
    let mut conv2 = RealtimeConversation::new();
    conv2.add_turn(RealtimeTurn {
        speaker: "user".to_string(),
        transcript: "Set a reminder for tomorrow at 9 AM".to_string(),
        duration_secs: 2.0,
        tool_calls: vec![],
        response_latency_ms: None,
    });
    conv2.add_turn(RealtimeTurn {
        speaker: "assistant".to_string(),
        transcript: "Sure, what would you like me to remind you about?".to_string(),
        duration_secs: 2.5,
        tool_calls: vec![],
        response_latency_ms: Some(280),
    });
    conv2.add_turn(RealtimeTurn {
        speaker: "user".to_string(),
        transcript: "Team meeting in conference room B".to_string(),
        duration_secs: 2.0,
        tool_calls: vec![],
        response_latency_ms: None,
    });
    conv2.add_turn(RealtimeTurn {
        speaker: "assistant".to_string(),
        transcript: "Done! I've set a reminder for tomorrow at 9 AM about your team meeting in conference room B.".to_string(),
        duration_secs: 4.0,
        tool_calls: vec!["create_reminder".to_string()],
        response_latency_ms: Some(450),
    });

    // Simulated conversation 3: High latency scenario
    let mut conv3 = RealtimeConversation::new();
    conv3.add_turn(RealtimeTurn {
        speaker: "user".to_string(),
        transcript: "Tell me a joke".to_string(),
        duration_secs: 1.5,
        tool_calls: vec![],
        response_latency_ms: None,
    });
    conv3.add_turn(RealtimeTurn {
        speaker: "assistant".to_string(),
        transcript: "Why do programmers prefer dark mode? Because light attracts bugs!".to_string(),
        duration_secs: 3.5,
        tool_calls: vec![],
        response_latency_ms: Some(750), // High latency
    });

    let conversations =
        vec![("Weather Query", conv1), ("Multi-turn Reminder", conv2), ("High Latency", conv3)];

    for (name, conv) in &conversations {
        println!("   {} ({:.1}s total):", name, conv.total_duration.as_secs_f64());
        println!("   Turns: {}", conv.turns.len());
        println!("   Tools: {:?}", conv.get_tool_trajectory());
        if let Some(latency) = conv.average_latency() {
            println!("   Avg latency: {}ms", latency);
        }
        println!();
    }

    // -------------------------------------------------------------------------
    // 2. Evaluate response latency
    // -------------------------------------------------------------------------
    println!("2. Evaluating response latency...\n");

    let criteria = RealtimeEvalCriteria::default();

    for (name, conv) in &conversations {
        if let Some(avg_latency) = conv.average_latency() {
            let passed = avg_latency <= criteria.max_latency_ms;
            let status = if passed { "PASS" } else { "FAIL" };
            println!(
                "   {}: {}ms avg latency (max: {}ms) [{}]",
                name, avg_latency, criteria.max_latency_ms, status
            );
        }
    }
    println!();

    // -------------------------------------------------------------------------
    // 3. Evaluate tool trajectory
    // -------------------------------------------------------------------------
    println!("3. Evaluating tool usage...\n");

    let trajectory_scorer = ToolTrajectoryScorer::new();

    let expected_tools = [
        ("Weather Query", vec!["get_weather"]),
        ("Multi-turn Reminder", vec!["create_reminder"]),
        ("High Latency", vec![]),
    ];

    for ((name, conv), (_, expected)) in conversations.iter().zip(expected_tools.iter()) {
        let actual = conv.get_tool_trajectory();

        let expected_uses: Vec<ToolUse> = expected.iter().map(|n| ToolUse::new(n)).collect();
        let actual_uses: Vec<ToolUse> = actual.iter().map(|n| ToolUse::new(n)).collect();

        let score = trajectory_scorer.score(&expected_uses, &actual_uses);
        let passed = score >= criteria.tool_accuracy;

        println!("   {}: expected {:?}, got {:?}", name, expected, actual);
        println!("      Score: {:.0}% [{}]\n", score * 100.0, if passed { "PASS" } else { "FAIL" });
    }

    // -------------------------------------------------------------------------
    // 4. Evaluate transcript quality
    // -------------------------------------------------------------------------
    println!("4. Evaluating transcript quality...\n");

    let response_scorer = ResponseScorer::with_config(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Jaccard,
        normalize: true,
        ignore_case: true,
        ignore_punctuation: true,
    });

    // Expected vs actual transcripts (simulating ASR accuracy)
    let transcript_tests = vec![
        (
            "The weather in San Francisco is 65 degrees and partly cloudy",
            "The weather in San Francisco is currently 65 degrees and partly cloudy.",
        ),
        (
            "Set reminder for team meeting",
            "Done! I've set a reminder for tomorrow at 9 AM about your team meeting in conference room B.",
        ),
    ];

    for (expected, actual) in &transcript_tests {
        let score = response_scorer.score(expected, actual);
        let passed = score >= criteria.transcript_similarity;

        println!("   Expected: \"{}\"", expected);
        println!("   Actual:   \"{}\"", actual);
        println!(
            "   Similarity: {:.0}% [{}]\n",
            score * 100.0,
            if passed { "PASS" } else { "FAIL" }
        );
    }

    // -------------------------------------------------------------------------
    // 5. LLM-judged evaluation for response quality
    // -------------------------------------------------------------------------
    println!("5. LLM-judged response quality...\n");

    let _ = dotenvy::dotenv();
    if let Ok(api_key) = std::env::var("GOOGLE_API_KEY") {
        let judge_model: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);
        let judge = LlmJudge::with_config(
            judge_model.clone(),
            LlmJudgeConfig { max_tokens: 512, temperature: 0.0 },
        );

        // Rubrics for voice assistant evaluation
        let rubrics = vec![
            Rubric::new("Naturalness", "Response sounds natural for voice")
                .with_weight(0.3)
                .with_levels(vec![
                    RubricLevel {
                        score: 1.0,
                        description: "Sounds like natural speech".to_string(),
                    },
                    RubricLevel { score: 0.5, description: "Somewhat robotic".to_string() },
                    RubricLevel { score: 0.0, description: "Very unnatural".to_string() },
                ]),
            Rubric::new("Conciseness", "Response is appropriately brief for voice")
                .with_weight(0.3)
                .with_levels(vec![
                    RubricLevel { score: 1.0, description: "Perfect length for voice".to_string() },
                    RubricLevel { score: 0.5, description: "Too long or too short".to_string() },
                    RubricLevel { score: 0.0, description: "Inappropriate length".to_string() },
                ]),
            Rubric::new("Helpfulness", "Response addresses user's need")
                .with_weight(0.4)
                .with_levels(vec![
                    RubricLevel { score: 1.0, description: "Fully helpful".to_string() },
                    RubricLevel { score: 0.5, description: "Partially helpful".to_string() },
                    RubricLevel { score: 0.0, description: "Not helpful".to_string() },
                ]),
        ];

        let rubric_config = RubricConfig { rubrics };

        // Evaluate assistant responses
        for (name, conv) in &conversations {
            let responses = conv.get_assistant_responses();
            if let Some(response) = responses.last() {
                let context = format!(
                    "Voice assistant conversation. User query: {}",
                    conv.turns.first().map(|t| t.transcript.as_str()).unwrap_or("")
                );

                let result = judge.evaluate_rubrics(response, &context, &rubric_config).await?;

                println!("   {} - Response: \"{}\"", name, &response[..response.len().min(60)]);
                println!("      Overall: {:.0}%", result.overall_score * 100.0);
                for score in &result.rubric_scores {
                    println!("      {}: {:.0}%", score.name, score.score * 100.0);
                }
                println!();
            }
        }
    } else {
        println!("   Skipped (GOOGLE_API_KEY not set)");
    }

    // -------------------------------------------------------------------------
    // 6. Full evaluation report
    // -------------------------------------------------------------------------
    println!("\n6. Summary Report\n");

    println!("   ┌─────────────────────┬─────────┬─────────┬─────────┐");
    println!("   │ Conversation        │ Latency │ Tools   │ Overall │");
    println!("   ├─────────────────────┼─────────┼─────────┼─────────┤");

    for (name, conv) in &conversations {
        let latency_ok =
            conv.average_latency().map(|l| l <= criteria.max_latency_ms).unwrap_or(true);
        let tools = conv.get_tool_trajectory();
        let tools_ok = !tools.is_empty() || conv.turns.iter().all(|t| t.tool_calls.is_empty());

        let latency_str = if latency_ok { "PASS" } else { "FAIL" };
        let tools_str = if tools_ok { "PASS" } else { "FAIL" };
        let overall = if latency_ok && tools_ok { "PASS" } else { "FAIL" };

        println!("   │ {:19} │ {:7} │ {:7} │ {:7} │", name, latency_str, tools_str, overall);
    }
    println!("   └─────────────────────┴─────────┴─────────┴─────────┘");

    // -------------------------------------------------------------------------
    // 7. Production setup
    // -------------------------------------------------------------------------
    println!("\n7. Production evaluator setup...\n");

    let eval_criteria = EvaluationCriteria {
        tool_trajectory_score: Some(1.0),
        response_similarity: Some(0.7),
        semantic_match_score: Some(0.8),
        ..Default::default()
    };

    let _evaluator = Evaluator::new(EvaluationConfig::with_criteria(eval_criteria));

    println!("   Production evaluator for realtime agents:");
    println!("   - Tool trajectory validation");
    println!("   - Transcript similarity checking");
    println!("   - Semantic response evaluation");
    println!("   - Custom latency thresholds");

    // -------------------------------------------------------------------------
    // Summary
    // -------------------------------------------------------------------------
    println!("\n=== Example Complete ===\n");
    println!("Key takeaways for realtime agent evaluation:");
    println!("  - Measure response latency (target: <500ms for natural conversation)");
    println!("  - Validate tool calls during voice sessions");
    println!("  - Check transcript accuracy (ASR quality)");
    println!("  - Use rubrics for voice-specific quality (naturalness, conciseness)");
    println!("  - Evaluate multi-turn conversation flow");
    println!("  - Consider audio-specific metrics (silence detection, interruption handling)");

    Ok(())
}
