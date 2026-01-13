//! Output formatting and callbacks for Ralph execution.
//!
//! This module provides debug-level-aware output through ADK callbacks.
//! The callbacks are designed to be added to agents for clean, configurable logging.
//!
//! ## Debug Levels
//!
//! - `Minimal`: Only errors and final status
//! - `Normal`: Human-readable progress (default) - shows task progress, phase changes
//! - `Verbose`: Detailed output with tool calls and responses
//! - `Debug`: Full debug output with all internal state

use crate::models::DebugLevel;
use adk_core::Part;
use colored::Colorize;

/// Output handler that respects debug levels.
///
/// This struct provides methods to create ADK callbacks that output
/// information based on the configured debug level.
#[derive(Debug, Clone)]
pub struct RalphOutput {
    level: DebugLevel,
}

impl Default for RalphOutput {
    fn default() -> Self {
        Self::new(DebugLevel::Normal)
    }
}

impl RalphOutput {
    /// Create a new output handler with the specified debug level.
    pub fn new(level: DebugLevel) -> Self {
        Self { level }
    }

    /// Get the current debug level.
    pub fn level(&self) -> DebugLevel {
        self.level
    }

    // =========================================================================
    // Direct output methods (for use outside callbacks)
    // =========================================================================

    /// Print a phase header (shown at Normal and above).
    pub fn phase(&self, name: &str) {
        if self.level.is_normal() {
            println!("\n{} {}", "▶".bright_cyan(), name.bright_white().bold());
        }
    }

    /// Print a status message within a phase (shown at Normal and above).
    pub fn status(&self, message: &str) {
        if self.level.is_normal() {
            println!("  {} {}", "•".bright_black(), message);
        }
    }

    /// Print a phase completion message (shown at Normal and above).
    pub fn phase_complete(&self, message: &str) {
        if self.level.is_normal() {
            println!("  {} {}", "✓".bright_green(), message.green());
        }
    }

    /// Print a list item (shown at Normal and above).
    pub fn list_item(&self, message: &str) {
        if self.level.is_normal() {
            println!("    {} {}", "─".bright_black(), message);
        }
    }

    /// Print a task start message (shown at Normal and above).
    pub fn task_start(&self, task_id: &str, title: &str) {
        if self.level.is_normal() {
            println!("  {} {} - {}", "→".bright_blue(), task_id.cyan(), title);
        }
    }

    /// Print a task completion message (shown at Normal and above).
    pub fn task_complete(&self, task_id: &str, success: bool) {
        if self.level.is_normal() {
            if success {
                println!("  {} {} completed", "✓".bright_green(), task_id.green());
            } else {
                println!("  {} {} failed", "✗".bright_red(), task_id.red());
            }
        }
    }

    /// Print iteration progress (shown at Normal and above).
    pub fn iteration(&self, current: u32, max: usize) {
        if self.level.is_normal() {
            println!(
                "  {} iteration {}/{}",
                "○".bright_black(),
                current,
                max
            );
        }
    }

    /// Print a tool call (shown at Verbose and above).
    pub fn tool_call(&self, name: &str, args: &serde_json::Value) {
        if self.level.is_verbose() {
            println!(
                "\n  {} {}",
                "🔧".bright_blue(),
                name.bright_white().bold()
            );
            if let Ok(pretty) = serde_json::to_string_pretty(args) {
                for line in pretty.lines() {
                    println!("     {}", line.bright_black());
                }
            }
        }
    }

    /// Print a tool response (shown at Verbose and above).
    pub fn tool_response(&self, name: &str, response: &serde_json::Value) {
        if self.level.is_verbose() {
            let resp_str = serde_json::to_string(response).unwrap_or_default();
            let display = if resp_str.len() > 300 {
                format!("{}...", &resp_str[..300])
            } else {
                resp_str
            };
            println!("     {} {}", "←".green(), display.bright_black());
        } else if self.level.is_debug() {
            println!("     {} {} response:", "←".green(), name.green());
            if let Ok(pretty) = serde_json::to_string_pretty(response) {
                for line in pretty.lines() {
                    println!("       {}", line.bright_black());
                }
            }
        }
    }

    /// Print LLM text output (shown at Verbose and above).
    pub fn llm_text(&self, text: &str) {
        if self.level.is_verbose() && !text.trim().is_empty() {
            println!("\n  {} {}", "💭".bright_magenta(), text.trim());
        }
    }

    /// Print debug information (shown at Debug only).
    pub fn debug(&self, context: &str, message: &str) {
        if self.level.is_debug() {
            println!(
                "  {} [{}] {}",
                "🐛".bright_yellow(),
                context.bright_black(),
                message
            );
        }
    }

    /// Print an error (always shown).
    pub fn error(&self, message: &str) {
        eprintln!("{} {}", "✗ Error:".bright_red().bold(), message);
    }

    /// Print a warning (shown at Normal and above).
    pub fn warn(&self, message: &str) {
        if self.level.is_normal() {
            println!("{} {}", "⚠".bright_yellow(), message.yellow());
        }
    }

    /// Print success message (always shown).
    pub fn success(&self, message: &str) {
        println!("{} {}", "✓".bright_green(), message.green());
    }

    /// Print the startup banner (shown at Normal and above).
    pub fn banner(&self) {
        if self.level.is_normal() {
            println!(
                "{}",
                r#"
  ____       _       _     
 |  _ \ __ _| |_ __ | |__  
 | |_) / _` | | '_ \| '_ \ 
 |  _ < (_| | | |_) | | | |
 |_| \_\__,_|_| .__/|_| |_|
              |_|          
"#
                .cyan()
            );
            println!(
                "{}",
                "Multi-Agent Autonomous Development System".bright_white()
            );
            println!();
        }
    }

    /// Print final summary (always shown except minimal only shows status).
    pub fn summary(&self, iterations: u32, tasks_completed: usize, tasks_total: usize, success: bool) {
        if self.level.is_minimal() {
            // Minimal: just the result
            if success {
                println!("✓ Complete: {}/{} tasks", tasks_completed, tasks_total);
            } else {
                println!("✗ Incomplete: {}/{} tasks in {} iterations", tasks_completed, tasks_total, iterations);
            }
        } else {
            // Normal and above: formatted summary
            println!();
            println!("{}", "─".repeat(50).bright_black());
            if success {
                println!(
                    "{} {} tasks completed in {} iterations",
                    "✓".bright_green(),
                    tasks_completed.to_string().green(),
                    iterations
                );
            } else {
                println!(
                    "{} {}/{} tasks completed in {} iterations",
                    "⚠".bright_yellow(),
                    tasks_completed,
                    tasks_total,
                    iterations
                );
            }
            println!("{}", "─".repeat(50).bright_black());
        }
    }
}

/// Process an event stream part and output based on debug level.
///
/// This is a helper for processing events from the agent run loop.
pub fn process_event_part(output: &RalphOutput, part: &Part) {
    match part {
        Part::FunctionCall { name, args, .. } => {
            output.tool_call(name, args);
        }
        Part::FunctionResponse { function_response, .. } => {
            output.tool_response(&function_response.name, &function_response.response);
        }
        Part::Text { text } => {
            output.llm_text(text);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_level_checks() {
        let minimal = RalphOutput::new(DebugLevel::Minimal);
        assert!(minimal.level().is_minimal());
        assert!(!minimal.level().is_normal());
        assert!(!minimal.level().is_verbose());
        assert!(!minimal.level().is_debug());

        let normal = RalphOutput::new(DebugLevel::Normal);
        assert!(!normal.level().is_minimal());
        assert!(normal.level().is_normal());
        assert!(!normal.level().is_verbose());
        assert!(!normal.level().is_debug());

        let verbose = RalphOutput::new(DebugLevel::Verbose);
        assert!(!verbose.level().is_minimal());
        assert!(verbose.level().is_normal());
        assert!(verbose.level().is_verbose());
        assert!(!verbose.level().is_debug());

        let debug = RalphOutput::new(DebugLevel::Debug);
        assert!(!debug.level().is_minimal());
        assert!(debug.level().is_normal());
        assert!(debug.level().is_verbose());
        assert!(debug.level().is_debug());
    }

    #[test]
    fn test_output_default() {
        let output = RalphOutput::default();
        assert_eq!(output.level(), DebugLevel::Normal);
    }
}
