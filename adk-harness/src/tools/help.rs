//! `harness_help`: the harness explains itself on demand (spec 3.1 §7.25).

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::manifest::HarnessManifest;

/// The name of the help tool.
pub const HARNESS_HELP: &str = "harness_help";

/// Explains the harness: its commands, its classes in each mode, its packs and what its
/// safety mechanisms cover (spec 3.1 §7.25).
///
/// It reads the manifest generated when the harness was built and the help pages the
/// surface supplied, never the workspace or the network. It is class `Read`, declared in
/// every mode, and returns at most 32 KiB inside `<harness-help topic="…" version="…">`.
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
///
/// let tool = adk_harness::HarnessHelpTool::default();
/// assert_eq!(tool.name(), "harness_help");
/// assert!(tool.is_read_only());
/// ```
#[derive(Debug, Default)]
pub struct HarnessHelpTool {
    manifest: Arc<OnceLock<HarnessManifest>>,
    pages: BTreeMap<String, String>,
}

impl HarnessHelpTool {
    /// A tool that answers from `manifest` once it is set and from `pages` by name.
    pub(crate) fn new(
        manifest: Arc<OnceLock<HarnessManifest>>,
        pages: BTreeMap<String, String>,
    ) -> Self {
        Self { manifest, pages }
    }
}

fn unknown_topic(topic: &str, known: &[String]) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "harness.help.unknown_topic",
        format!("no help topic '{topic}'; topics: {}", known.join(", ")),
    )
}

#[async_trait]
impl Tool for HarnessHelpTool {
    fn name(&self) -> &str {
        HARNESS_HELP
    }

    /// Answers from the harness's own text.
    fn egress(&self, _args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        adk_core::egress::ToolEgress::None
    }

    fn description(&self) -> &str {
        "Explain this harness: the commands the person can type, what each class of call does \
         in each mode, the packs, memory and what the safety mechanisms cover. Call it with no \
         topic for the topic list, or with a topic such as `commands`, `cli <subcommand>`, \
         `manifest` or a page name."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "commands, cli <subcommand>, manifest, or a page name; empty for the list"
                }
            }
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let topic = args.get("topic").and_then(Value::as_str);
        let Some(manifest) = self.manifest.get() else {
            return Ok(Value::String(
                "The harness is still starting; its manifest is not ready.".into(),
            ));
        };
        match manifest.help(topic, &self.pages) {
            Some(text) => Ok(Value::String(text)),
            None => {
                let mut known = manifest.help_topics.clone();
                known.extend(self.pages.keys().cloned());
                Err(unknown_topic(topic.unwrap_or_default(), &known))
            }
        }
    }
}
