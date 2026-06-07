//! Eval Showcase — demonstrates the eval-competitive-parity features from `adk-eval`.
//!
//! This example exercises all 10 new capabilities without requiring API keys or
//! external services. Features that need an LLM (StructuredJudge, ConversationScorer)
//! demonstrate the parsing/config logic directly.
//!
//! Run: `cargo run -p eval-showcase`

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use adk_eval::ab_comparator::wilcoxon_signed_rank;
use adk_eval::annotation::{AnnotationRecord, AnnotationStore, HumanVerdict};
use adk_eval::baseline::BaselineStore;
use adk_eval::conversation_scorer::{ConversationMetrics, ConversationScorerConfig};
use adk_eval::cost_tracker::CostTracker;
use adk_eval::embedding_scorer::cosine_similarity;
use adk_eval::junit_reporter::JunitReporter;
use adk_eval::report::{EvaluationReport, EvaluationResult, Failure};
use adk_eval::structured_judge::{StructuredVerdict, extract_json_from_text};
use adk_eval::test_generator::{EvalCaseMetadata, GeneratorConfig, TestGenerator};
use adk_eval::trace_analyzer::{ToolCallRecord, TraceAnalyzer};

use adk_core::{Content, Event, LlmRequest, LlmResponse, Part};

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           ADK-Eval Competitive Parity Showcase              ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    demo_structured_judge();
    demo_cost_tracker();
    demo_trace_analyzer();
    demo_baseline_store();
    demo_junit_reporter();
    demo_annotation_store();
    demo_wilcoxon();
    demo_test_generator();
    demo_conversation_scorer();
    demo_embedding_scorer();

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                   All demos complete!                       ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. StructuredJudge — extract_json_from_text() and verdict parsing
// ─────────────────────────────────────────────────────────────────────────────

fn demo_structured_judge() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  1. StructuredJudge — JSON extraction from LLM text");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Simulated LLM output with JSON in markdown fences
    let llm_output = r#"After careful analysis, here is my evaluation:

```json
{"score": 0.85, "reasoning": "The response captures the key points but misses some nuance.", "verdict": "partial"}
```

That concludes my assessment."#;

    let json = extract_json_from_text(llm_output).expect("should extract JSON");
    let verdict: StructuredVerdict =
        serde_json::from_value(json).expect("should deserialize verdict");

    println!("  Input:     LLM text with embedded JSON in markdown fences");
    println!("  Score:     {:.2}", verdict.score);
    println!("  Verdict:   {:?}", verdict.verdict);
    println!("  Reasoning: {}", verdict.reasoning);
    println!();

    // Raw JSON extraction
    let raw = r#"{"score": 1.0, "reasoning": "Perfect match", "verdict": "pass"}"#;
    let json2 = extract_json_from_text(raw).expect("raw JSON extraction");
    let verdict2: StructuredVerdict = serde_json::from_value(json2).unwrap();
    println!("  Raw JSON:  score={:.2}, verdict={:?}", verdict2.score, verdict2.verdict);

    // Fallback when no JSON present
    let garbage = "I think it's pretty good overall.";
    let result = extract_json_from_text(garbage);
    println!("  No JSON:   extract returns None = {}", result.is_none());
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. CostTracker — compute_cost() with token counts
// ─────────────────────────────────────────────────────────────────────────────

fn demo_cost_tracker() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  2. CostTracker — token cost computation");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let tracker = CostTracker::new();

    // Compute cost for known models
    let models = [
        ("gpt-4o", 2000u64, 800u64),
        ("gemini-2.5-flash", 5000, 1200),
        ("claude-3.5-sonnet", 3000, 1000),
    ];

    for (model, prompt, completion) in models {
        match tracker.compute_cost(model, prompt, completion) {
            Some(cost) => {
                println!(
                    "  {:<20} prompt={:<5} completion={:<5} → ${:.6}",
                    model, prompt, completion, cost
                );
            }
            None => {
                println!("  {:<20} (pricing not configured)", model);
            }
        }
    }

    // Unknown model returns None
    let unknown = tracker.compute_cost("unknown-model-xyz", 100, 50);
    println!();
    println!("  Unknown model cost: {:?}", unknown);
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. TraceAnalyzer — detect redundant tool calls and loops
// ─────────────────────────────────────────────────────────────────────────────

fn demo_trace_analyzer() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  3. TraceAnalyzer — efficiency scoring");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let analyzer = TraceAnalyzer::new();

    // Sequence with redundant calls
    let calls = vec![
        ToolCallRecord {
            name: "read_file".into(),
            args: serde_json::json!({"path": "config.toml"}),
        },
        ToolCallRecord {
            name: "read_file".into(),
            args: serde_json::json!({"path": "config.toml"}),
        },
        ToolCallRecord {
            name: "read_file".into(),
            args: serde_json::json!({"path": "config.toml"}),
        },
        ToolCallRecord {
            name: "write_file".into(),
            args: serde_json::json!({"path": "output.txt", "content": "hello"}),
        },
        ToolCallRecord { name: "search".into(), args: serde_json::json!({"query": "rust async"}) },
        ToolCallRecord { name: "search".into(), args: serde_json::json!({"query": "rust async"}) },
    ];

    let analysis = analyzer.analyze_tool_calls(&calls);

    println!("  Tool calls:      {}", analysis.total_tool_calls);
    println!("  Unique tools:    {}", analysis.unique_tools);
    println!("  Useful calls:    {}", analysis.useful_tool_calls);
    println!("  Efficiency:      {:.1}%", analysis.efficiency_score * 100.0);
    println!("  Diagnostics:");
    for diag in &analysis.diagnostics {
        println!("    [{:?}] {}", diag.pattern_type, diag.description);
    }
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. BaselineStore — save, load, check regressions
// ─────────────────────────────────────────────────────────────────────────────

fn demo_baseline_store() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  4. BaselineStore — regression detection");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let dir = tempfile::tempdir().expect("create temp dir");
    let baseline_path = dir.path().join("baseline.json");
    let store = BaselineStore::new(&baseline_path);

    // Save baseline metrics
    let mut metrics = HashMap::new();
    let mut accuracy = HashMap::new();
    accuracy.insert("case_1".to_string(), 0.95);
    accuracy.insert("case_2".to_string(), 0.88);
    metrics.insert("accuracy".to_string(), accuracy);

    store.save("weather_agent_eval", &metrics).expect("save baseline");
    println!("  Saved baseline: accuracy case_1=0.95, case_2=0.88");

    // Load it back
    let loaded = store.load().expect("load").expect("baseline exists");
    println!("  Loaded baseline: eval_set_id={}", loaded.eval_set_id);

    // Check with slightly degraded metrics
    let mut degraded = metrics.clone();
    degraded.get_mut("accuracy").unwrap().insert("case_1".to_string(), 0.78);

    let regressions = store.check_regressions(&degraded, 0.05).expect("check");
    println!("  Degraded case_1 to 0.78 (tolerance=0.05):");
    for reg in &regressions {
        println!(
            "    REGRESSION: {} / {} — baseline={:.2}, current={:.2}, delta={:.2}",
            reg.metric_name, reg.case_id, reg.baseline_value, reg.current_value, reg.delta
        );
    }

    // No regression when within tolerance
    let mut within_tolerance = metrics.clone();
    within_tolerance.get_mut("accuracy").unwrap().insert("case_1".to_string(), 0.92);
    let no_reg = store.check_regressions(&within_tolerance, 0.05).expect("check");
    println!("  Within tolerance (0.92): regressions={}", no_reg.len());
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. JunitReporter — generate JUnit XML from evaluation report
// ─────────────────────────────────────────────────────────────────────────────

fn demo_junit_reporter() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  5. JunitReporter — CI-friendly XML output");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let results = vec![
        EvaluationResult::passed(
            "greeting_test",
            HashMap::from([("quality".to_string(), 0.95)]),
            Duration::from_millis(120),
        ),
        EvaluationResult::failed(
            "math_test",
            HashMap::from([("accuracy".to_string(), 0.3)]),
            vec![Failure::new(
                "accuracy",
                serde_json::Value::String("42".to_string()),
                serde_json::Value::String("41".to_string()),
                0.3,
                0.8,
            )],
            Duration::from_millis(200),
        ),
    ];

    let started_at = chrono::Utc::now();
    let report = EvaluationReport::new("demo-run-001", results, started_at);
    let xml = JunitReporter::generate(&report, "eval_showcase_suite").expect("generate XML");

    println!("  Generated JUnit XML ({} bytes):", xml.len());
    println!();
    // Print first few lines
    for line in xml.lines().take(8) {
        println!("    {line}");
    }
    if xml.lines().count() > 8 {
        println!("    ...");
    }
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. AnnotationStore — JSONL export and import
// ─────────────────────────────────────────────────────────────────────────────

fn demo_annotation_store() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  6. AnnotationStore — JSONL export/import");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    let dir = tempfile::tempdir().expect("create temp dir");
    let jsonl_path = dir.path().join("annotations.jsonl");

    // Create sample annotation records manually (simulates export + human review)
    let records = vec![
        AnnotationRecord {
            case_id: "case_weather".to_string(),
            input: "What's the weather in Paris?".to_string(),
            expected_response: Some("The weather in Paris is sunny, 22°C.".to_string()),
            actual_response: Some("It's sunny and warm in Paris today.".to_string()),
            verdict: Some(HumanVerdict {
                score: 0.85,
                reasoning: "Captures the key info but omits exact temperature.".to_string(),
                annotator_id: "reviewer_1".to_string(),
            }),
        },
        AnnotationRecord {
            case_id: "case_joke".to_string(),
            input: "Tell me a joke".to_string(),
            expected_response: Some(
                "Why don't scientists trust atoms? Because they make up everything!".to_string(),
            ),
            actual_response: Some(
                "Here's a joke: What do you call a fake noodle? An impasta!".to_string(),
            ),
            verdict: Some(HumanVerdict {
                score: 0.7,
                reasoning: "Different joke but still funny and appropriate.".to_string(),
                annotator_id: "reviewer_2".to_string(),
            }),
        },
    ];

    // Write JSONL
    let content: String =
        records.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>().join("\n");
    std::fs::write(&jsonl_path, &content).expect("write jsonl");
    println!("  Exported {} records to JSONL", records.len());

    // Import with valid case IDs
    let valid_ids: HashSet<String> =
        ["case_weather", "case_joke", "case_unknown"].iter().map(|s| s.to_string()).collect();
    let (imported, warnings) = AnnotationStore::import(&jsonl_path, &valid_ids).expect("import");

    println!("  Imported {} records, {} warnings", imported.len(), warnings.len());
    for rec in &imported {
        if let Some(verdict) = &rec.verdict {
            println!(
                "    {} — score={:.2}, annotator={}",
                rec.case_id, verdict.score, verdict.annotator_id
            );
        }
    }
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. Wilcoxon signed-rank test — A/B statistical comparison
// ─────────────────────────────────────────────────────────────────────────────

fn demo_wilcoxon() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  7. Wilcoxon Signed-Rank — A/B significance testing");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Scenario 1: Clear winner (agent A consistently better)
    let diffs_clear = vec![0.3, 0.25, 0.4, 0.35, 0.28, 0.32, 0.38, 0.27, 0.31, 0.29];
    let p_clear = wilcoxon_signed_rank(&diffs_clear);
    println!("  Consistent positive diffs: p={:.6} (significant={})", p_clear, p_clear < 0.05);

    // Scenario 2: Mixed results (no clear winner)
    let diffs_mixed = vec![0.1, -0.15, 0.05, -0.08, 0.12, -0.1, 0.03, -0.07, 0.09, -0.11];
    let p_mixed = wilcoxon_signed_rank(&diffs_mixed);
    println!("  Mixed differences:         p={:.6} (significant={})", p_mixed, p_mixed < 0.05);

    // Scenario 3: All zeros (no difference)
    let diffs_zero = vec![0.0, 0.0, 0.0, 0.0, 0.0];
    let p_zero = wilcoxon_signed_rank(&diffs_zero);
    println!("  All zeros:                 p={:.6} (significant={})", p_zero, p_zero < 0.05);

    // Empty input
    let p_empty = wilcoxon_signed_rank(&[]);
    println!("  Empty input:               p={:.6}", p_empty);
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. TestGenerator — generate_from_events() without LLM
// ─────────────────────────────────────────────────────────────────────────────

fn demo_test_generator() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  8. TestGenerator — generate from events (no LLM)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // We need a mock LLM for the TestGenerator struct (not actually called)
    let mock_llm = std::sync::Arc::new(MockLlm);

    let config = GeneratorConfig { cases_per_description: 3, include_tool_expectations: true };
    let generator = TestGenerator::with_config(mock_llm, config);

    // Simulate production events
    let mut events = Vec::new();

    // User asks about weather
    let mut user_event = Event::new("inv_1");
    user_event.llm_response = LlmResponse {
        content: Some(Content::new("user").with_text("What's the weather in Tokyo?")),
        ..Default::default()
    };
    events.push(user_event);

    // Agent calls a tool and responds
    let mut model_event = Event::new("inv_1");
    model_event.llm_response = LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::FunctionCall {
                    name: "get_weather".to_string(),
                    args: serde_json::json!({"city": "Tokyo", "units": "metric"}),
                    id: Some("call_1".to_string()),
                    thought_signature: None,
                },
                Part::Text { text: "The weather in Tokyo is 18°C and partly cloudy.".to_string() },
            ],
        }),
        ..Default::default()
    };
    events.push(model_event);

    let cases = generator.generate_from_events(&events).expect("generate from events");

    println!("  Generated {} eval case(s) from {} event(s)", cases.len(), events.len());
    for case in &cases {
        println!("  Case ID: {}", case.eval_id);
        println!("  Tags: {:?}", case.tags);
        for turn in &case.conversation {
            println!("    Turn: input=\"{}\"", turn.user_content.get_text());
            if let Some(resp) = &turn.final_response {
                println!("          response=\"{}\"", resp.get_text());
            }
            if let Some(data) = &turn.intermediate_data {
                for tool in &data.tool_uses {
                    println!("          tool_call: {}({})", tool.name, tool.args);
                }
            }
        }
    }

    // Show metadata struct
    let meta = EvalCaseMetadata { generated: true, source: Some("events".to_string()) };
    println!("  Metadata: {:?}", meta);
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. ConversationScorer — config creation and metrics struct
// ─────────────────────────────────────────────────────────────────────────────

fn demo_conversation_scorer() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  9. ConversationScorer — config and metrics");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Show configuration
    let config = ConversationScorerConfig::default();
    println!("  Default config:");
    println!("    context_retention_threshold: {}", config.context_retention_threshold);
    println!("    goal_completion_threshold:   {}", config.goal_completion_threshold);
    println!("    coherence_threshold:         {}", config.coherence_threshold);
    println!("    topic_drift_threshold:       {}", config.topic_drift_threshold);
    println!();

    // Custom config
    let custom = ConversationScorerConfig {
        context_retention_threshold: 0.8,
        goal_completion_threshold: 0.9,
        coherence_threshold: 0.75,
        topic_drift_threshold: 0.85,
    };
    let json = serde_json::to_string_pretty(&custom).unwrap();
    println!("  Custom config JSON:");
    for line in json.lines() {
        println!("    {line}");
    }
    println!();

    // Show metrics struct
    let metrics = ConversationMetrics {
        context_retention: 0.92,
        goal_completion: 0.87,
        coherence: 0.95,
        topic_drift: 0.88,
    };
    println!("  Sample metrics:");
    println!("    context_retention: {:.2}", metrics.context_retention);
    println!("    goal_completion:   {:.2}", metrics.goal_completion);
    println!("    coherence:         {:.2}", metrics.coherence);
    println!("    topic_drift:       {:.2}", metrics.topic_drift);
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. EmbeddingScorer — cosine_similarity() with sample vectors
// ─────────────────────────────────────────────────────────────────────────────

fn demo_embedding_scorer() {
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  10. EmbeddingScorer — cosine similarity");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Identical vectors → similarity 1.0
    let v1 = vec![0.5_f32, 0.8, 0.3, 0.9, 0.1];
    let sim_identical = cosine_similarity(&v1, &v1);
    println!("  Identical vectors:     {:.6}", sim_identical);

    // Similar vectors (small perturbation)
    let v2 = vec![0.52_f32, 0.78, 0.31, 0.88, 0.12];
    let sim_similar = cosine_similarity(&v1, &v2);
    println!("  Similar vectors:       {:.6}", sim_similar);

    // Orthogonal vectors
    let a = vec![1.0_f32, 0.0, 0.0];
    let b = vec![0.0_f32, 1.0, 0.0];
    let sim_orthogonal = cosine_similarity(&a, &b);
    println!("  Orthogonal vectors:    {:.6}", sim_orthogonal);

    // Opposite vectors (clamped to 0.0)
    let c = vec![1.0_f32, 0.0, 0.0];
    let d = vec![-1.0_f32, 0.0, 0.0];
    let sim_opposite = cosine_similarity(&c, &d);
    println!("  Opposite vectors:      {:.6} (clamped from -1.0)", sim_opposite);

    // Zero vector edge case
    let zero = vec![0.0_f32, 0.0, 0.0];
    let sim_zero = cosine_similarity(&v1[..3], &zero);
    println!("  Zero vector:           {:.6}", sim_zero);

    // Dimension mismatch
    let short = vec![1.0_f32, 2.0];
    let long = vec![1.0_f32, 2.0, 3.0];
    let sim_mismatch = cosine_similarity(&short, &long);
    println!("  Dimension mismatch:    {:.6}", sim_mismatch);
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// Mock LLM for TestGenerator (never actually called in generate_from_events)
// ─────────────────────────────────────────────────────────────────────────────

struct MockLlm;

#[async_trait::async_trait]
impl adk_core::Llm for MockLlm {
    fn name(&self) -> &str {
        "mock-llm"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        unimplemented!("MockLlm is not meant to be called in this example")
    }
}
