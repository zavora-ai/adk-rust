//! Guardrail integration for LlmAgent
//!
//! This module provides guardrail support when the `guardrails` feature is enabled.

use adk_core::{Content, Event, Result};

#[cfg(feature = "guardrails")]
use adk_core::Part;

#[cfg(feature = "guardrails")]
pub use adk_guardrail::{
    ContentFilter, ContentFilterConfig, GUARDRAIL_WARNINGS_KEY, Guardrail, GuardrailError,
    GuardrailExecutor, GuardrailFailure, GuardrailPhase, GuardrailResult, GuardrailSet,
    INPUT_BLOCKED_CODE, OUTPUT_BLOCKED_CODE, PiiRedactor, PiiType, Severity,
};

#[cfg(feature = "guardrails")]
pub use adk_guardrail::SchemaValidator;

/// Placeholder type when guardrails feature is disabled
#[cfg(not(feature = "guardrails"))]
pub struct GuardrailSet;

#[cfg(not(feature = "guardrails"))]
impl GuardrailSet {
    /// Create an empty guardrail set (no-op when feature is disabled).
    pub fn new() -> Self {
        Self
    }
    /// Returns `true` (always empty when feature is disabled).
    pub fn is_empty(&self) -> bool {
        true
    }
}

#[cfg(not(feature = "guardrails"))]
impl Default for GuardrailSet {
    fn default() -> Self {
        Self::new()
    }
}

/// What a guardrail set is screening.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Phase {
    /// A user message.
    Input,
    /// What the agent produces.
    Output,
}

#[cfg(feature = "guardrails")]
impl From<Phase> for GuardrailPhase {
    fn from(phase: Phase) -> Self {
        match phase {
            Phase::Input => Self::Input,
            Phase::Output => Self::Output,
        }
    }
}

/// Content a guardrail set let through, with the low-severity failures it passed.
pub(crate) struct Screened {
    pub(crate) content: Content,
    pub(crate) warnings: Warnings,
}

impl Screened {
    pub(crate) fn unscreened(content: Content) -> Self {
        Self { content, warnings: Warnings::default() }
    }
}

/// The low-severity failures of the output guardrails that screened one event.
#[derive(Default)]
pub(crate) struct Warnings {
    #[cfg(feature = "guardrails")]
    failures: Vec<GuardrailFailure>,
}

impl Warnings {
    /// Adds the warnings of another screening of the same event.
    pub(crate) fn extend(&mut self, other: Warnings) {
        #[cfg(feature = "guardrails")]
        self.failures.extend(other.failures);
        #[cfg(not(feature = "guardrails"))]
        let _ = other;
    }

    /// Records the failures on `event`, under `GUARDRAIL_WARNINGS_KEY`, so the client and the
    /// session see what the guardrails flagged but passed.
    pub(crate) fn record_on(&self, event: &mut Event) {
        #[cfg(feature = "guardrails")]
        if !self.failures.is_empty() {
            event.provider_metadata.insert(
                GUARDRAIL_WARNINGS_KEY.to_string(),
                serde_json::to_string(&self.failures).unwrap_or_default(),
            );
        }
        #[cfg(not(feature = "guardrails"))]
        let _ = event;
    }
}

/// Runs `guardrails` over `content`, failing with a structured block (category
/// `InvalidInput`, code `guardrail.input_blocked` or `guardrail.output_blocked`, and the
/// blocking guardrail's name and severity in the error details).
#[cfg(feature = "guardrails")]
pub(crate) async fn enforce_guardrails(
    guardrails: &GuardrailSet,
    content: &Content,
    phase: Phase,
) -> Result<Screened> {
    if guardrails.is_empty() {
        return Ok(Screened::unscreened(content.clone()));
    }
    let screening = GuardrailExecutor::enforce(guardrails, content, phase.into(), false).await?;
    Ok(Screened { content: screening.content, warnings: Warnings { failures: screening.warnings } })
}

#[cfg(not(feature = "guardrails"))]
pub(crate) async fn enforce_guardrails(
    _guardrails: &GuardrailSet,
    content: &Content,
    _phase: Phase,
) -> Result<Screened> {
    Ok(Screened::unscreened(content.clone()))
}

/// Where a text shown to output guardrails sits in the model turn.
#[cfg(feature = "guardrails")]
enum Slot {
    /// A text part.
    Text(usize),
    /// A text item of a provider-native assistant message kept for replay.
    Native { part: usize, item: usize },
    /// Any other part of a final turn, shown to the guardrails unchanged.
    Other(usize),
}

/// The text items of a provider-native assistant message that a turn carries for replay, such
/// as an OpenAI Responses `commentary` message written beside a tool call.
#[cfg(feature = "guardrails")]
fn native_message_texts(payload: &serde_json::Value) -> Vec<(usize, String)> {
    if payload.get("type").and_then(serde_json::Value::as_str) != Some("message")
        || payload.get("role").and_then(serde_json::Value::as_str) != Some("assistant")
    {
        return Vec::new();
    }
    payload
        .get("content")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .enumerate()
                .filter_map(|(item, entry)| {
                    entry.get("text").and_then(serde_json::Value::as_str).map(|t| (item, t.into()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Screens a model turn with output guardrails before it reaches the client or the session.
///
/// A final turn is shown to the guardrails whole and `validate` decides. A `preamble` turn —
/// one that also calls tools, or that the provider paused — is screened with
/// `validate_preamble`, and only its text is shown, so its function calls pass untouched. In
/// both, the text of a provider-native assistant message carried for replay is shown as a text
/// part and written back into the message. A block fails the turn; a transform rewrites the
/// text in place.
#[cfg(feature = "guardrails")]
pub(crate) async fn screen_model_output(
    guardrails: &GuardrailSet,
    mut content: Content,
    preamble: bool,
) -> Result<Screened> {
    if guardrails.is_empty() {
        return Ok(Screened::unscreened(content));
    }
    let mut slots = Vec::new();
    let mut view = Vec::new();
    for (index, part) in content.parts.iter().enumerate() {
        let native = match part {
            Part::ServerToolCall { server_tool_call } => native_message_texts(server_tool_call),
            _ => Vec::new(),
        };
        if !native.is_empty() {
            for (item, text) in native {
                slots.push(Slot::Native { part: index, item });
                view.push(Part::Text { text });
            }
        } else if let Part::Text { text } = part {
            slots.push(Slot::Text(index));
            view.push(Part::Text { text: text.clone() });
        } else if !preamble {
            slots.push(Slot::Other(index));
            view.push(part.clone());
        }
    }
    if preamble && view.is_empty() {
        return Ok(Screened::unscreened(content));
    }

    let shown = Content { role: content.role.clone(), parts: view };
    let screening =
        GuardrailExecutor::enforce(guardrails, &shown, GuardrailPhase::Output, preamble).await?;
    let mut screened = screening.content.parts;

    if screened.len() == slots.len() {
        for (slot, part) in slots.iter().zip(screened) {
            match *slot {
                Slot::Text(index) | Slot::Other(index) => content.parts[index] = part,
                Slot::Native { part: index, item } => {
                    let text = part.text().unwrap_or_default().to_string();
                    if let Part::ServerToolCall { server_tool_call } = &mut content.parts[index]
                        && let Some(entry) = server_tool_call
                            .get_mut("content")
                            .and_then(|items| items.get_mut(item))
                    {
                        entry["text"] = serde_json::Value::String(text);
                    }
                }
            }
        }
    } else if !preamble {
        // A guardrail that restructured the turn replaces it, as it always has.
        content.parts = screened;
    } else {
        // The guardrail returned a different number of parts: its text takes the first text
        // slot and the other slots are emptied, so no unscreened text survives and the calls
        // are kept.
        let joined: String =
            screened.drain(..).filter_map(|part| part.text().map(str::to_string)).collect();
        let mut first = Some(joined);
        let mut emptied = Vec::new();
        for slot in &slots {
            let text = first.take().unwrap_or_default();
            match *slot {
                Slot::Text(index) => {
                    if text.is_empty() {
                        emptied.push(index);
                    }
                    content.parts[index] = Part::Text { text };
                }
                Slot::Native { part: index, item } => {
                    if let Part::ServerToolCall { server_tool_call } = &mut content.parts[index]
                        && let Some(entry) = server_tool_call
                            .get_mut("content")
                            .and_then(|items| items.get_mut(item))
                    {
                        entry["text"] = serde_json::Value::String(text);
                    }
                }
                Slot::Other(_) => {}
            }
        }
        let mut index = 0;
        content.parts.retain(|_| {
            let keep = !emptied.contains(&index);
            index += 1;
            keep
        });
    }
    Ok(Screened { content, warnings: Warnings { failures: screening.warnings } })
}

#[cfg(not(feature = "guardrails"))]
pub(crate) async fn screen_model_output(
    _guardrails: &GuardrailSet,
    content: Content,
    _preamble: bool,
) -> Result<Screened> {
    Ok(Screened::unscreened(content))
}

#[cfg(feature = "guardrails")]
pub use adk_guardrail::{
    DeniedArgumentPattern, PathAllowList, ResultTextGuardrail, ToolCallDecision, ToolGuardrail,
    ToolGuardrailResult, ToolGuardrailSet, ToolResultDecision, ToolResultGuardrailResult,
};

/// Placeholder type when the guardrails feature is disabled.
#[cfg(not(feature = "guardrails"))]
pub struct ToolGuardrailSet;

#[cfg(not(feature = "guardrails"))]
impl ToolGuardrailSet {
    /// Create an empty tool guardrail set (no-op when the feature is disabled).
    pub fn new() -> Self {
        Self
    }
    /// Returns `true` (always empty when the feature is disabled).
    pub fn is_empty(&self) -> bool {
        true
    }
}

#[cfg(not(feature = "guardrails"))]
impl Default for ToolGuardrailSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of screening a tool call, independent of whether guardrails are compiled in.
pub(crate) enum ToolScreening {
    /// The call may proceed with these arguments.
    Allow(serde_json::Value),
    /// The call is refused, with a reason to report back to the model.
    ///
    /// Never constructed without the `guardrails` feature, where screening is a no-op that always
    /// allows. The variant still exists so the call site is identical in both builds.
    #[cfg_attr(not(feature = "guardrails"), allow(dead_code))]
    Deny(String),
}

/// Screens a tool call against `guardrails` before it executes.
#[cfg(feature = "guardrails")]
pub(crate) async fn screen_tool_call(
    guardrails: &ToolGuardrailSet,
    tool_name: &str,
    args: &serde_json::Value,
) -> ToolScreening {
    if guardrails.is_empty() {
        return ToolScreening::Allow(args.clone());
    }

    match guardrails.evaluate(tool_name, args).await {
        ToolCallDecision::Allow { args } => ToolScreening::Allow(args),
        ToolCallDecision::Deny { guardrail, reason, severity } => ToolScreening::Deny(format!(
            "Tool '{tool_name}' blocked by guardrail '{guardrail}' ({severity:?}): {reason}"
        )),
    }
}

#[cfg(not(feature = "guardrails"))]
pub(crate) async fn screen_tool_call(
    _guardrails: &ToolGuardrailSet,
    _tool_name: &str,
    args: &serde_json::Value,
) -> ToolScreening {
    ToolScreening::Allow(args.clone())
}

/// Presents a tool guardrail set to the governed execution path as its screen.
pub(crate) struct GuardrailScreen<'a>(pub(crate) &'a ToolGuardrailSet);

#[async_trait::async_trait]
impl adk_core::ToolCallScreen for GuardrailScreen<'_> {
    async fn screen(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> std::result::Result<serde_json::Value, String> {
        match screen_tool_call(self.0, tool_name, args).await {
            ToolScreening::Allow(args) => Ok(args),
            ToolScreening::Deny(reason) => Err(reason),
        }
    }
}

/// Screens the function responses of one call's result with the agent's tool guardrails
/// (`ToolGuardrail::validate_result`) before the model and the session see them.
///
/// A withheld result is replaced by `adk_core::result_blocked_response`, and its inline and
/// file data are dropped with it. Returns the screened content and the reason for each
/// withheld response, which the caller logs under the payload rule.
#[cfg(feature = "guardrails")]
pub(crate) async fn screen_tool_result(
    guardrails: &ToolGuardrailSet,
    tool_name: &str,
    args: &serde_json::Value,
    mut content: Content,
) -> (Content, Vec<String>) {
    use adk_core::ToolResultScreen as _;

    let mut withheld = Vec::new();
    if guardrails.is_empty() {
        return (content, withheld);
    }
    for part in &mut content.parts {
        let Part::FunctionResponse { function_response, .. } = part else { continue };
        match guardrails.screen_result(tool_name, args, &function_response.response).await {
            Ok(result) => function_response.response = result,
            Err(reason) => {
                function_response.response = adk_core::result_blocked_response(tool_name, &reason);
                function_response.inline_data.clear();
                function_response.file_data.clear();
                withheld.push(reason);
            }
        }
    }
    (content, withheld)
}

#[cfg(not(feature = "guardrails"))]
pub(crate) async fn screen_tool_result(
    _guardrails: &ToolGuardrailSet,
    _tool_name: &str,
    _args: &serde_json::Value,
    content: Content,
) -> (Content, Vec<String>) {
    (content, Vec::new())
}
