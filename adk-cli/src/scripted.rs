//! Hidden hooks for binary end-to-end tests: a scripted model and a tool that needs
//! approval (spec 3.1 §11, adk-cli row).

use std::sync::Arc;

use adk_core::{Llm, Tool, ToolContext};
use adk_model::{ScriptedLlm, ScriptedResponse};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

/// One scripted model reply.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
enum Reply {
    Text(String),
    Call(Call),
    Calls(Vec<Call>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    #[serde(default)]
    args: Value,
}

/// The model `--model-script` describes: a JSON array of replies, inline or in a file.
pub fn model(script: &str) -> Result<Arc<dyn Llm>> {
    let json = if script.trim_start().starts_with('[') {
        script.to_string()
    } else {
        std::fs::read_to_string(script)
            .with_context(|| format!("failed to read the model script {script}"))?
    };
    let replies: Vec<Reply> = serde_json::from_str(&json).context(
        "--model-script must be a JSON array of {\"text\"}, {\"call\"} or {\"calls\"} replies",
    )?;
    let script = replies.into_iter().map(|reply| match reply {
        Reply::Text(text) => ScriptedResponse::text(text),
        Reply::Call(call) => ScriptedResponse::function_call(call.name, call.args),
        Reply::Calls(calls) => {
            ScriptedResponse::function_calls(calls.into_iter().map(|call| (call.name, call.args)))
        }
    });
    Ok(Arc::new(ScriptedLlm::new("scripted", script)))
}

/// Records a note and reports it back. The chat agent requires approval for it.
pub struct RecordNote;

impl RecordNote {
    /// The tool's name.
    pub const NAME: &'static str = "record_note";
}

#[async_trait]
impl Tool for RecordNote {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Records a short note for the user. Every call needs the user's approval."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "note": { "type": "string", "description": "The note to record" } },
            "required": ["note"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "recorded": args.get("note").cloned().unwrap_or(Value::Null) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::LlmRequest;
    use futures::StreamExt;

    #[tokio::test]
    async fn a_script_answers_calls_in_order() {
        let model = model(
            r#"[{"call": {"name": "record_note", "args": {"note": "hi"}}},
                {"calls": [{"name": "a"}, {"name": "b", "args": {"x": 1}}]},
                {"text": "Recorded."}]"#,
        )
        .unwrap();
        let mut replies = Vec::new();
        for _ in 0..3 {
            let mut stream =
                model.generate_content(LlmRequest::new("scripted", vec![]), false).await.unwrap();
            replies.push(stream.next().await.unwrap().unwrap().content.unwrap());
        }
        assert!(replies[0].has_function_calls());
        assert_eq!(replies[1].parts.len(), 2);
        assert_eq!(replies[2].parts[0].text(), Some("Recorded."));
        assert!(super::model(r#"[{"speak": "x"}]"#).is_err());
    }
}
