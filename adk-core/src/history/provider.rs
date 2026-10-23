//! Provider replay and explicit ownership projection without extending `Part`.
use crate::{Content, Part, ProviderOrigin};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const REPLAY_KEY: &str = "__adk_provider_replay";

// These records exist only in reconstructed history. Provider events retain their
// original content and metadata. The public Part enum and its fields stay intact.
#[derive(Serialize, Deserialize)]
struct Replay {
    origin: ProviderOrigin,
    #[serde(flatten)]
    data: ReplayData,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReplayData {
    Opaque { part: Part },
    Signature { name: String, args: Value, id: Option<String>, signature: String },
}

pub(crate) fn is_provider_replay(part: &Part) -> bool {
    matches!(part, Part::ServerToolCall { server_tool_call } if server_tool_call.get(REPLAY_KEY).is_some())
}

fn record(part: &Part) -> Option<Replay> {
    let Part::ServerToolCall { server_tool_call } = part else { return None };
    serde_json::from_value(server_tool_call.get(REPLAY_KEY)?.clone()).ok()
}

pub(crate) fn replay_part(part: &Part) -> Option<Part> {
    match record(part)?.data {
        ReplayData::Opaque { part } if opaque(&part) && !is_provider_replay(&part) => Some(part),
        _ => None,
    }
}

fn encode(origin: &ProviderOrigin, data: ReplayData) -> Part {
    Part::ServerToolCall {
        server_tool_call: json!({REPLAY_KEY: Replay { origin: origin.clone(), data }}),
    }
}

fn unsigned_id(id: &Option<String>) -> Option<&str> {
    id.as_deref().filter(|id| !id.starts_with(crate::GENERATED_CALL_ID_PREFIX))
}

fn matches_call(part: &Part, name: &str, args: &Value, id: &Option<String>) -> bool {
    matches!(part, Part::FunctionCall { name: call_name, args: call_args, id: call_id, .. }
        if call_name == name && call_args == args && unsigned_id(call_id) == unsigned_id(id))
}

fn contains(parts: &[Part], candidate: &Part) -> bool {
    parts.iter().enumerate().any(|(index, part)| {
        if part == candidate || replay_part(part).as_ref() == Some(candidate) {
            return true;
        }
        if let Some(Replay { data: ReplayData::Signature { name, args, id, signature }, .. }) = record(part)
            && let Some(call) = index.checked_sub(1).and_then(|index| parts.get(index))
            && matches_call(call, &name, &args, &id)
        {
            return matches_call(candidate, &name, &args, &id)
                && matches!(candidate, Part::FunctionCall { thought_signature: Some(candidate_signature), .. }
                    if candidate_signature == &signature);
        }
        false
    })
}

/// Adds provider replay parts to a history copy, preserving source order.
///
/// `conversation_history_parts` may appear at the metadata root or under `openai`.
/// Parts already in content are not appended again. A valid `provider_origin`
/// binds opaque data to its provider, model and account. Internal ownership records
/// use namespaced JSON in the existing `ServerToolCall` variant; function calls
/// remain ordinary calls with a separate signature record immediately after them.
/// Use [`for_provider`] to restore owned data before sending. Converters omit
/// unresolved records through [`Content::for_model`].
///
/// Without ownership, same-invocation replay stays unchanged. Applications that
/// switch providers or accounts must stamp origins and explicitly project history.
/// Source content and metadata are never modified.
///
/// # Example
///
/// ```rust
/// use adk_core::{Content, Part};
/// use adk_core::history::with_provider_parts;
/// use serde_json::json;
/// let metadata = json!({"conversation_history_parts":[{
///     "server_tool_call":{"type":"reasoning","encrypted_content":"opaque"}
/// }]});
/// let source = Content::new("model").with_text("Answer");
/// let replay = with_provider_parts(&source, Some(&metadata));
/// assert_eq!(source.parts.len(), 1);
/// assert_eq!(replay.parts.len(), 2);
/// assert!(matches!(replay.parts[1], Part::ServerToolCall { .. }));
/// ```
pub fn with_provider_parts(content: &Content, metadata: Option<&Value>) -> Content {
    let mut content = content.clone();
    let parts = metadata
        .and_then(|metadata| {
            metadata.get("conversation_history_parts").or_else(|| {
                metadata.get("openai").and_then(|openai| openai.get("conversation_history_parts"))
            })
        })
        .and_then(Value::as_array);
    for part in parts.into_iter().flatten() {
        if let Ok(part) = serde_json::from_value::<Part>(part.clone())
            && !contains(&content.parts, &part)
        {
            content.parts.push(part);
        }
    }
    let Some(origin) = provider_origin(metadata) else { return content };
    let mut parts = Vec::with_capacity(content.parts.len());
    for mut part in content.parts {
        if is_provider_replay(&part) {
            // Never rebind an existing record to a different caller's ownership.
            parts.push(part);
        } else if opaque(&part) {
            parts.push(encode(&origin, ReplayData::Opaque { part }));
        } else if let Part::FunctionCall { name, args, id, thought_signature } = &mut part
            && let Some(signature) = thought_signature.take()
        {
            let data = ReplayData::Signature {
                name: name.clone(),
                args: args.clone(),
                id: id.clone(),
                signature,
            };
            parts.push(part);
            parts.push(encode(&origin, data));
        } else {
            parts.push(part);
        }
    }
    content.parts = parts;
    content
}

/// Reads valid, nonempty provider, model and account identities from metadata.
pub fn provider_origin(metadata: Option<&Value>) -> Option<ProviderOrigin> {
    metadata
        .and_then(|metadata| metadata.get("provider_origin"))
        .and_then(|origin| serde_json::from_value::<ProviderOrigin>(origin.clone()).ok())
        .filter(valid_origin)
}

fn valid_origin(origin: &ProviderOrigin) -> bool {
    !origin.provider.is_empty() && !origin.model.is_empty() && !origin.scope.is_empty()
}

fn opaque(part: &Part) -> bool {
    matches!(
        part,
        Part::Thinking { .. } | Part::ServerToolCall { .. } | Part::ServerToolResponse { .. }
    )
}

/// Projects history for one provider, model and authorization owner.
///
/// Matching opaque records replay in place; foreign and unowned opaque data is
/// omitted. Text, ordinary function calls and tool results remain. A matching
/// signature record may restore only the immediately preceding existing call's
/// signature, never its name, arguments or ID, and never creates a call.
/// Invalid or nested internal records are omitted. Persisted events are unchanged.
///
/// # Example
///
/// ```rust
/// use adk_core::{Content, ProviderOrigin};
/// use adk_core::history::for_provider;
/// let owner = ProviderOrigin { provider: "provider".into(), model: "model".into(), scope: "account".into() };
/// let mut contents = vec![Content::new("model").with_thinking("Unowned").with_text("Answer")];
/// for_provider(&mut contents, &owner);
/// assert_eq!(contents[0].parts.len(), 1);
/// ```
pub fn for_provider(contents: &mut Vec<Content>, origin: &ProviderOrigin) {
    for content in contents.iter_mut() {
        let mut parts = Vec::with_capacity(content.parts.len());
        // Keep input adjacency: dropping a foreign/malformed record must not make
        // a later signature record apply to an earlier, unrelated call.
        let mut previous_call = false;
        for mut part in std::mem::take(&mut content.parts) {
            if is_provider_replay(&part) {
                if let Some(Replay { origin: owner, data }) = record(&part)
                    && valid_origin(origin)
                    && owner == *origin
                {
                    match data {
                        ReplayData::Opaque { part }
                            if opaque(&part) && !is_provider_replay(&part) =>
                        {
                            parts.push(part)
                        }
                        ReplayData::Signature { name, args, id, signature } if previous_call => {
                            if let Some(call) = parts.last_mut()
                                && matches_call(call, &name, &args, &id)
                                && let Part::FunctionCall { thought_signature, .. } = call
                            {
                                *thought_signature = Some(signature);
                            }
                        }
                        _ => {}
                    }
                }
                previous_call = false;
            } else {
                previous_call = matches!(part, Part::FunctionCall { .. });
                if opaque(&part) {
                    continue;
                }
                if let Part::FunctionCall { thought_signature, .. } = &mut part {
                    *thought_signature = None;
                }
                parts.push(part);
            }
        }
        content.parts = parts;
    }
    contents.retain(|content| !content.parts.is_empty());
}

#[cfg(test)]
mod tests;
