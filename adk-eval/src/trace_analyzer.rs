//! Execution trace analysis for detecting inefficiencies.
//!
//! The [`TraceAnalyzer`] inspects agent event streams to identify redundant tool calls,
//! execution loops, and other patterns that waste tokens or time. It produces a
//! [`TraceAnalysis`] summary with an efficiency score and per-pattern diagnostics.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::trace_analyzer::{TraceAnalyzer, ToolCallRecord};
//! use serde_json::json;
//!
//! let analyzer = TraceAnalyzer::new();
//! let calls = vec![
//!     ToolCallRecord { name: "read_file".into(), args: json!({"path": "a.txt"}) },
//!     ToolCallRecord { name: "read_file".into(), args: json!({"path": "a.txt"}) },
//!     ToolCallRecord { name: "write_file".into(), args: json!({"path": "b.txt"}) },
//! ];
//! let analysis = analyzer.analyze_tool_calls(&calls);
//! assert!(analysis.efficiency_score < 1.0);
//! ```

use adk_core::Event;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A single tool call record for direct analysis without full Events.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    /// Name of the tool that was called.
    pub name: String,
    /// Arguments passed to the tool as JSON.
    pub args: serde_json::Value,
}

/// A detected trace inefficiency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDiagnostic {
    /// Type of inefficiency pattern detected.
    pub pattern_type: TracePattern,
    /// Tool names involved in the pattern.
    pub tool_names: Vec<String>,
    /// Number of times the pattern occurred.
    pub occurrence_count: usize,
    /// Human-readable description of the issue.
    pub description: String,
}

/// Types of trace inefficiency patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TracePattern {
    /// Same tool called consecutively with identical arguments.
    RedundantCall,
    /// Repeated sequence of tool calls forming a loop.
    ExecutionLoop,
    /// Tool called many times suggesting retry issues.
    ExcessiveRetries,
}

/// Summary of trace analysis results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceAnalysis {
    /// Total number of tool calls in the event stream.
    pub total_tool_calls: usize,
    /// Number of unique tools used.
    pub unique_tools: usize,
    /// Number of useful (non-redundant, non-loop) tool calls.
    pub useful_tool_calls: usize,
    /// Efficiency score in [0.0, 1.0]: useful_calls / total_calls (1.0 when total is 0).
    pub efficiency_score: f64,
    /// Detected inefficiency patterns.
    pub diagnostics: Vec<TraceDiagnostic>,
}

/// Analyzes agent execution traces for inefficiencies.
///
/// The analyzer inspects tool call sequences to detect:
/// - **Redundant calls**: consecutive calls with the same tool name AND same arguments
/// - **Execution loops**: sequences of 3+ repeated tool-call patterns
///
/// # Example
///
/// ```rust,ignore
/// use adk_eval::TraceAnalyzer;
///
/// let analyzer = TraceAnalyzer::new();
/// let analysis = analyzer.analyze(&events);
/// println!("Efficiency: {:.1}%", analysis.efficiency_score * 100.0);
/// ```
pub struct TraceAnalyzer;

impl TraceAnalyzer {
    /// Creates a new `TraceAnalyzer`.
    pub fn new() -> Self {
        Self
    }

    /// Analyze an event stream for trace inefficiencies.
    ///
    /// Extracts tool calls from events and delegates to [`Self::analyze_tool_calls`].
    pub fn analyze(&self, events: &[Event]) -> TraceAnalysis {
        let calls = Self::extract_tool_calls(events);
        self.analyze_tool_calls(&calls)
    }

    /// Analyze a sequence of tool call records directly.
    ///
    /// This is useful for testing without constructing full Event objects.
    pub fn analyze_tool_calls(&self, calls: &[ToolCallRecord]) -> TraceAnalysis {
        let total_tool_calls = calls.len();

        if total_tool_calls == 0 {
            return TraceAnalysis {
                total_tool_calls: 0,
                unique_tools: 0,
                useful_tool_calls: 0,
                efficiency_score: 1.0,
                diagnostics: Vec::new(),
            };
        }

        let unique_tools = {
            let mut set = HashSet::new();
            for call in calls {
                set.insert(call.name.as_str());
            }
            set.len()
        };

        let redundant_diagnostics = Self::detect_redundant_calls(calls);
        let loop_diagnostics = Self::detect_loops(calls);

        let redundant_count: usize = redundant_diagnostics.iter().map(|d| d.occurrence_count).sum();
        let loop_count: usize = loop_diagnostics.iter().map(|d| d.occurrence_count).sum();

        let wasted = redundant_count + loop_count;
        let useful_tool_calls = total_tool_calls.saturating_sub(wasted);

        let efficiency_score = useful_tool_calls as f64 / total_tool_calls as f64;

        let mut diagnostics = Vec::new();
        diagnostics.extend(redundant_diagnostics);
        diagnostics.extend(loop_diagnostics);

        TraceAnalysis {
            total_tool_calls,
            unique_tools,
            useful_tool_calls,
            efficiency_score,
            diagnostics,
        }
    }

    /// Extract tool calls from events by scanning for `FunctionCall` parts.
    fn extract_tool_calls(events: &[Event]) -> Vec<ToolCallRecord> {
        let mut calls = Vec::new();
        for event in events {
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    if let adk_core::Part::FunctionCall { name, args, .. } = part {
                        calls.push(ToolCallRecord { name: name.clone(), args: args.clone() });
                    }
                }
            }
        }
        calls
    }

    /// Detect redundant consecutive calls — same tool name AND same arguments.
    ///
    /// Two consecutive tool calls are redundant if they have the same tool name
    /// and their arguments are equal (JSON equality).
    fn detect_redundant_calls(calls: &[ToolCallRecord]) -> Vec<TraceDiagnostic> {
        if calls.len() < 2 {
            return Vec::new();
        }

        let mut diagnostics: Vec<TraceDiagnostic> = Vec::new();

        let mut i = 0;
        while i < calls.len() - 1 {
            if calls[i].name == calls[i + 1].name && calls[i].args == calls[i + 1].args {
                // Count how many consecutive duplicates follow
                let tool_name = calls[i].name.clone();
                let mut count = 0;
                let mut j = i + 1;
                while j < calls.len()
                    && calls[j].name == calls[i].name
                    && calls[j].args == calls[i].args
                {
                    count += 1;
                    j += 1;
                }

                diagnostics.push(TraceDiagnostic {
                    pattern_type: TracePattern::RedundantCall,
                    tool_names: vec![tool_name.clone()],
                    occurrence_count: count,
                    description: format!(
                        "Tool '{}' called {} consecutive time(s) with identical arguments",
                        tool_name, count
                    ),
                });

                i = j;
            } else {
                i += 1;
            }
        }

        diagnostics
    }

    /// Detect execution loops — sequences of 3+ repeated tool-call patterns.
    ///
    /// Uses a sliding window approach: for each possible pattern length (1..=n/3),
    /// checks if a sequence of tool call names repeats 3+ times consecutively.
    fn detect_loops(calls: &[ToolCallRecord]) -> Vec<TraceDiagnostic> {
        if calls.len() < 3 {
            return Vec::new();
        }

        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        let n = names.len();
        let mut diagnostics: Vec<TraceDiagnostic> = Vec::new();
        let mut covered: Vec<bool> = vec![false; n];

        // Try pattern lengths from 1 up to n/3 (need at least 3 repetitions)
        for pattern_len in 1..=(n / 3) {
            let mut i = 0;
            while i + pattern_len * 3 <= n {
                if covered[i] {
                    i += 1;
                    continue;
                }

                let pattern = &names[i..i + pattern_len];
                let mut repetitions = 1;
                let mut j = i + pattern_len;

                while j + pattern_len <= n && &names[j..j + pattern_len] == pattern {
                    repetitions += 1;
                    j += pattern_len;
                }

                if repetitions >= 3 {
                    let loop_tool_names: Vec<String> =
                        pattern.iter().map(|s| (*s).to_string()).collect();

                    // Mark covered indices to avoid double-counting
                    // The wasted iterations are repetitions - 1 (first occurrence is useful)
                    let wasted_iterations = (repetitions - 1) * pattern_len;
                    for item in
                        covered.iter_mut().take(i + repetitions * pattern_len).skip(i + pattern_len)
                    {
                        *item = true;
                    }

                    diagnostics.push(TraceDiagnostic {
                        pattern_type: TracePattern::ExecutionLoop,
                        tool_names: loop_tool_names.clone(),
                        occurrence_count: wasted_iterations,
                        description: format!(
                            "Pattern {:?} repeated {} times ({} wasted iterations)",
                            loop_tool_names, repetitions, wasted_iterations
                        ),
                    });

                    i = j;
                } else {
                    i += 1;
                }
            }
        }

        diagnostics
    }
}

impl Default for TraceAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_empty_calls() {
        let analyzer = TraceAnalyzer::new();
        let analysis = analyzer.analyze_tool_calls(&[]);
        assert_eq!(analysis.total_tool_calls, 0);
        assert_eq!(analysis.unique_tools, 0);
        assert_eq!(analysis.useful_tool_calls, 0);
        assert_eq!(analysis.efficiency_score, 1.0);
        assert!(analysis.diagnostics.is_empty());
    }

    #[test]
    fn test_no_redundancy() {
        let analyzer = TraceAnalyzer::new();
        let calls = vec![
            ToolCallRecord { name: "read_file".into(), args: json!({"path": "a.txt"}) },
            ToolCallRecord { name: "write_file".into(), args: json!({"path": "b.txt"}) },
            ToolCallRecord { name: "read_file".into(), args: json!({"path": "c.txt"}) },
        ];
        let analysis = analyzer.analyze_tool_calls(&calls);
        assert_eq!(analysis.total_tool_calls, 3);
        assert_eq!(analysis.unique_tools, 2);
        assert_eq!(analysis.useful_tool_calls, 3);
        assert_eq!(analysis.efficiency_score, 1.0);
        assert!(analysis.diagnostics.is_empty());
    }

    #[test]
    fn test_redundant_calls_detected() {
        let analyzer = TraceAnalyzer::new();
        let calls = vec![
            ToolCallRecord { name: "read_file".into(), args: json!({"path": "a.txt"}) },
            ToolCallRecord { name: "read_file".into(), args: json!({"path": "a.txt"}) },
            ToolCallRecord { name: "write_file".into(), args: json!({"path": "b.txt"}) },
        ];
        let analysis = analyzer.analyze_tool_calls(&calls);
        assert_eq!(analysis.total_tool_calls, 3);
        assert_eq!(analysis.useful_tool_calls, 2);
        assert!(analysis.efficiency_score < 1.0);
        assert!(!analysis.diagnostics.is_empty());
    }

    #[test]
    fn test_same_tool_different_args_not_redundant() {
        let analyzer = TraceAnalyzer::new();
        let calls = vec![
            ToolCallRecord { name: "read_file".into(), args: json!({"path": "a.txt"}) },
            ToolCallRecord { name: "read_file".into(), args: json!({"path": "b.txt"}) },
        ];
        let analysis = analyzer.analyze_tool_calls(&calls);
        assert_eq!(analysis.useful_tool_calls, 2);
        assert_eq!(analysis.efficiency_score, 1.0);
        assert!(analysis.diagnostics.is_empty());
    }

    #[test]
    fn test_loop_detection() {
        let analyzer = TraceAnalyzer::new();
        // Pattern "a" repeated 4 times
        let calls = vec![
            ToolCallRecord { name: "check".into(), args: json!({}) },
            ToolCallRecord { name: "check".into(), args: json!({}) },
            ToolCallRecord { name: "check".into(), args: json!({}) },
            ToolCallRecord { name: "check".into(), args: json!({}) },
        ];
        let analysis = analyzer.analyze_tool_calls(&calls);
        assert_eq!(analysis.total_tool_calls, 4);
        // Should detect redundancy and/or loops
        assert!(analysis.useful_tool_calls < 4);
        assert!(analysis.efficiency_score < 1.0);
    }

    #[test]
    fn test_multi_tool_loop_detection() {
        let analyzer = TraceAnalyzer::new();
        // Pattern ["read", "write"] repeated 3 times
        let calls = vec![
            ToolCallRecord { name: "read".into(), args: json!({"x": 1}) },
            ToolCallRecord { name: "write".into(), args: json!({"y": 2}) },
            ToolCallRecord { name: "read".into(), args: json!({"x": 1}) },
            ToolCallRecord { name: "write".into(), args: json!({"y": 2}) },
            ToolCallRecord { name: "read".into(), args: json!({"x": 1}) },
            ToolCallRecord { name: "write".into(), args: json!({"y": 2}) },
        ];
        let analysis = analyzer.analyze_tool_calls(&calls);
        assert_eq!(analysis.total_tool_calls, 6);
        // Loop pattern detected — some iterations are wasted
        assert!(analysis.useful_tool_calls < 6);
        assert!(analysis.efficiency_score < 1.0);
    }

    #[test]
    fn test_analyze_events() {
        use adk_core::{Content, Event, Part};

        let analyzer = TraceAnalyzer::new();
        let mut event1 = Event::new("inv-1");
        event1.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: json!({"city": "NYC"}),
                id: None,
                thought_signature: None,
            }],
        });

        let mut event2 = Event::new("inv-1");
        event2.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: json!({"city": "NYC"}),
                id: None,
                thought_signature: None,
            }],
        });

        let analysis = analyzer.analyze(&[event1, event2]);
        assert_eq!(analysis.total_tool_calls, 2);
        assert_eq!(analysis.unique_tools, 1);
        // Redundant call detected
        assert_eq!(analysis.useful_tool_calls, 1);
        assert_eq!(analysis.efficiency_score, 0.5);
    }

    #[test]
    fn test_single_call() {
        let analyzer = TraceAnalyzer::new();
        let calls = vec![ToolCallRecord { name: "search".into(), args: json!({"query": "hello"}) }];
        let analysis = analyzer.analyze_tool_calls(&calls);
        assert_eq!(analysis.total_tool_calls, 1);
        assert_eq!(analysis.unique_tools, 1);
        assert_eq!(analysis.useful_tool_calls, 1);
        assert_eq!(analysis.efficiency_score, 1.0);
    }

    #[test]
    fn test_efficiency_score_bounds() {
        let analyzer = TraceAnalyzer::new();
        // All redundant: same call 5 times
        let calls = vec![
            ToolCallRecord { name: "ping".into(), args: json!({}) },
            ToolCallRecord { name: "ping".into(), args: json!({}) },
            ToolCallRecord { name: "ping".into(), args: json!({}) },
            ToolCallRecord { name: "ping".into(), args: json!({}) },
            ToolCallRecord { name: "ping".into(), args: json!({}) },
        ];
        let analysis = analyzer.analyze_tool_calls(&calls);
        assert!(analysis.efficiency_score >= 0.0);
        assert!(analysis.efficiency_score <= 1.0);
        assert!(analysis.useful_tool_calls <= analysis.total_tool_calls);
    }
}
