//! The model-facing text: the Monty language briefing and the tool catalog.
//!
//! The [`CodeRuntime`](adk_agent::codeact::CodeRuntime) owns how the language
//! and tools are described to the model. This module produces both: a static
//! description of the Monty Python subset and the return contract, and a
//! per-invocation tool catalog rendered as [`call_tool`](TOOL_DISPATCH_FN)
//! invocations — the single way a script calls a tool.

use adk_core::Tool;
use serde_json::{Map, Value};

/// The Monty language briefing injected verbatim into the system prompt.
///
/// It covers only what is *specific to Monty* and cannot be inferred: the Python
/// *subset* Monty accepts, the language mechanism for reporting a result (the
/// script's last expression), and the single tool-calling convention. The OS
/// access a script is granted is appended separately by
/// [`OsAccess::prompt_section`](crate::OsAccess), and the set of result tags and
/// their meanings is owned by the framework's `CODEACT_SYSTEM_PROMPT`, which is
/// injected ahead of this text — so this briefing deliberately does not repeat
/// them.
pub(crate) const MONTY_PROMPT: &str = "\
You write Python that runs on Monty — a fast, sandboxed, Rust-native Python
interpreter. Emit exactly ONE fenced code block per turn (```python ... ```).

Monty runs a SUBSET of Python:
- No `class` definitions and no `match` statements.
- No third-party libraries and no imports beyond a small standard library
  (`sys` , `os`, `typing`, `asyncio`, `re`, `datetime`, `json`, `pathlib`).
- Filesystem, environment, and clock access are restricted by the host policy
  described in the OS access section below.
- Functions, loops, comprehensions, conditionals, f-strings, and the common
  builtins (`len`, `sum`, `min`, `max`, `sorted`, `range`, `enumerate`,
  `int`, `float`, `str`, `dict`, `list`, ...) all work.

Monty has no top-level `return`: the result of your script is the value of its
LAST EXPRESSION. Make that final expression the tagged result dict described
above.

Invoke a tool ONLY with the built-in `call_tool` function, passing the tool name
and a single dict of arguments — this is the single way to call a tool, and tool
names are never in scope as bare callables:

    result = call_tool(\"tool_name\", {\"arg\": value, ...})

Put every tool argument inside that dict (never as a keyword argument to
`call_tool` itself), and compose the results with ordinary Python. Call tools
synchronously — do NOT use `await`.";

/// The one built-in function a script uses to invoke a tool:
/// `call_tool("tool-name", {"arg": value, ...})`.
///
/// This is the *only* tool-calling convention. The tool name is a string literal
/// and every argument is a string-keyed entry in a single dict. Rendering every
/// tool through it (rather than as a bare `def name(...)` with keyword
/// parameters) means:
///
/// - the real tool name rides inside the serialized continuation, so it survives
///   suspend/resume with no host-side name table;
/// - a tool may carry *any* name — not a valid Python identifier (`"fetch-cart"`),
///   a Python keyword, or even `"call_tool"` itself;
/// - argument names are plain dict keys, so they too can be anything and the
///   driver binds them by name *exactly* (no positional-order inference).
pub(crate) const TOOL_DISPATCH_FN: &str = "call_tool";

/// Render one tool as a [`call_tool`](TOOL_DISPATCH_FN) usage line, preceded by
/// its description as a comment.
///
/// Every tool — whatever its name — is invoked the same way:
/// `call_tool("name", {"arg": value, ...})`. The argument dict is illustrative:
/// keys are listed required-first (reconstructed from the tool's JSON schema)
/// with placeholder types, purely for readability. The runtime does not rely on
/// the ordering — the CodeAct driver binds the dict's entries onto the tool's
/// parameters by name via `bind_call_args`.
pub(crate) fn tool_entry(tool: &dyn Tool) -> String {
    let decl = tool.declaration();
    let params = decl.get("parameters");
    let properties = params.and_then(|p| p.get("properties")).and_then(Value::as_object);
    let required: Vec<String> = params
        .and_then(|p| p.get("required"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    // Required params first (in their declared order), then any optional ones.
    let mut ordered: Vec<String> = required.clone();
    if let Some(props) = properties {
        for key in props.keys() {
            if !ordered.contains(key) {
                ordered.push(key.clone());
            }
        }
    }

    // Each argument is a string-keyed dict entry, so a key can be anything — no
    // Python-identifier or keyword constraints apply.
    let rendered: Vec<String> = ordered
        .iter()
        .map(|p| {
            let ty = py_type(properties, p);
            let placeholder =
                if required.contains(p) { format!("<{ty}>") } else { format!("<{ty}, optional>") };
            format!("{p:?}: {placeholder}")
        })
        .collect();

    let name = tool.name();
    let call = if rendered.is_empty() {
        format!("{TOOL_DISPATCH_FN}({name:?})")
    } else {
        format!("{TOOL_DISPATCH_FN}({name:?}, {{{}}})", rendered.join(", "))
    };

    let description = one_line(tool.description());
    if description.is_empty() {
        format!("{call}\n")
    } else {
        format!("# {}\n{call}\n", escape_comment(&description))
    }
}

/// Strip anything that would break out of a single-line `#` comment.
fn escape_comment(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// Map a JSON Schema `type` to a Python type-hint name for the placeholder.
fn py_type(properties: Option<&Map<String, Value>>, name: &str) -> &'static str {
    let ty = properties
        .and_then(|props| props.get(name))
        .and_then(|schema| schema.get("type"))
        .and_then(Value::as_str);
    match ty {
        Some("string") => "str",
        Some("integer") => "int",
        Some("number") => "float",
        Some("boolean") => "bool",
        Some("array") => "list",
        Some("object") => "dict",
        _ => "Any",
    }
}

/// Collapse whitespace runs (including newlines) so a multi-line tool
/// description fits on the catalog's one-line comment.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct DemoTool;

    #[async_trait]
    impl Tool for DemoTool {
        fn name(&self) -> &str {
            "search"
        }
        fn description(&self) -> &str {
            "Search the web\nfor a query."
        }
        fn parameters_schema(&self) -> Option<Value> {
            Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["query"]
            }))
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn every_tool_is_rendered_as_a_call_tool_args_dict_with_collapsed_description() {
        let entry = tool_entry(&DemoTool);
        assert!(
            entry.contains("call_tool(\"search\", {\"query\": <str>, \"limit\": <int, optional>})"),
            "unexpected entry: {entry}"
        );
        // A tool is never offered as a bare callable.
        assert!(!entry.contains("def "), "tools must not be rendered as `def`: {entry}");
        // The multi-line description is collapsed onto a single comment line.
        assert!(entry.contains("# Search the web for a query."), "unexpected entry: {entry}");
    }

    /// A tool whose name is not a valid Python identifier.
    struct HyphenTool;

    #[async_trait]
    impl Tool for HyphenTool {
        fn name(&self) -> &str {
            "fetch-cart"
        }
        fn description(&self) -> &str {
            "Fetch a cart."
        }
        fn parameters_schema(&self) -> Option<Value> {
            Some(serde_json::json!({
                "type": "object",
                "properties": { "user_id": {"type": "string"} },
                "required": ["user_id"]
            }))
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn a_non_identifier_name_uses_the_same_call_tool_form() {
        let entry = tool_entry(&HyphenTool);
        assert!(
            entry.contains("call_tool(\"fetch-cart\", {\"user_id\": <str>})"),
            "unexpected entry: {entry}"
        );
        assert!(!entry.contains("def fetch-cart"), "no bare def for a hyphenated name: {entry}");
    }

    /// A tool with no parameters.
    struct NoArgTool;

    #[async_trait]
    impl Tool for NoArgTool {
        fn name(&self) -> &str {
            "ping"
        }
        fn description(&self) -> &str {
            ""
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn a_zero_parameter_tool_takes_no_args_dict() {
        let entry = tool_entry(&NoArgTool);
        assert!(entry.contains("call_tool(\"ping\")\n"), "unexpected entry: {entry}");
        assert!(!entry.contains("call_tool(\"ping\", {"), "zero-arg tool needs no dict: {entry}");
    }

    /// A tool with a parameter named like a Python keyword — a non-issue now that
    /// argument names are plain dict keys.
    struct KeywordParamTool;

    #[async_trait]
    impl Tool for KeywordParamTool {
        fn name(&self) -> &str {
            "classify"
        }
        fn description(&self) -> &str {
            "Classify input."
        }
        fn parameters_schema(&self) -> Option<Value> {
            Some(serde_json::json!({
                "type": "object",
                "properties": { "class": {"type": "string"} },
                "required": ["class"]
            }))
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn a_keyword_named_parameter_is_just_a_dict_key() {
        let entry = tool_entry(&KeywordParamTool);
        assert!(
            entry.contains("call_tool(\"classify\", {\"class\": <str>})"),
            "keyword-named param should be a plain string key: {entry}"
        );
    }
}
