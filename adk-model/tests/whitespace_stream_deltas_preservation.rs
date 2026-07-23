//! Preservation property tests for the whitespace-in-stream-deltas bugfix.
//!
//! Spec: `preserve-whitespace-stream-deltas`
//!
//! **Property 2: Preservation — Unchanged Behavior for Non-Buggy Streams**
//!
//! The fix (in `adk-model/src/tool_call_parser.rs`) only changes behavior for
//! streams that trigger the bug condition (whitespace-only deltas, or whitespace
//! that abuts tool-call markup). Every other behavior — tool-call detection and
//! parsing for the six text formats, buffering across split chunks, overflow
//! flush, name/args trimming, empty-buffer flush, and plain non-whitespace text
//! emission — must remain identical.
//!
//! These tests follow the observation-first methodology: they encode the ACTUAL
//! behavior of the UNFIXED code and MUST PASS on it. Because every generated
//! input deliberately avoids the bug condition (no whitespace-only deltas, no
//! whitespace at tool-call boundaries), `F(X) = F'(X)` holds and the same
//! assertions will continue to pass after the fix — that is the guarantee.
//!
//! **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6**

use std::collections::BTreeMap;

use adk_core::Part;
use adk_model::tool_call_parser::{BufferAction, ToolCallBuffer, parse_text_tool_calls};
use proptest::prelude::*;

/// The six text-based tool-call markup formats the parser supports.
#[derive(Debug, Clone, Copy)]
enum Fmt {
    Qwen,
    Llama,
    MistralNemo,
    DeepSeek,
    Gemma4,
    ActionTag,
}

fn arb_fmt() -> impl Strategy<Value = Fmt> {
    prop_oneof![
        Just(Fmt::Qwen),
        Just(Fmt::Llama),
        Just(Fmt::MistralNemo),
        Just(Fmt::DeepSeek),
        Just(Fmt::Gemma4),
        Just(Fmt::ActionTag),
    ]
}

/// A tool-call function name (identifier, no whitespace).
fn arb_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{1,15}".prop_map(|s| s.to_string())
}

/// Argument keys/values are non-whitespace tokens so the generated JSON and the
/// Gemma custom markup stay well-formed and comparable across formats.
fn arb_args() -> impl Strategy<Value = BTreeMap<String, String>> {
    prop::collection::btree_map("[a-z][a-z0-9_]{1,10}", "[a-zA-Z0-9]{1,12}", 0..3)
}

/// Build the expected `args` JSON object (string-valued) for comparison.
fn json_object(args: &BTreeMap<String, String>) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> =
        args.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    serde_json::Value::Object(map)
}

/// Build well-formed markup for a given format. No surrounding visible text is
/// added, so there are no `before`/`trailing` slices to trim — this keeps the
/// input strictly outside the bug condition.
fn build_markup(fmt: Fmt, name: &str, args: &BTreeMap<String, String>) -> String {
    let args_str = serde_json::to_string(&json_object(args)).unwrap();
    match fmt {
        Fmt::Qwen => {
            format!(r#"<tool_call>{{"name": "{name}", "arguments": {args_str}}}</tool_call>"#)
        }
        Fmt::Llama => {
            format!(r#"<|python_tag|>{{"name": "{name}", "parameters": {args_str}}}"#)
        }
        Fmt::MistralNemo => {
            format!(r#"[TOOL_CALLS][{{"name": "{name}", "arguments": {args_str}}}]"#)
        }
        Fmt::DeepSeek => {
            format!("```json\n{{\"name\": \"{name}\", \"arguments\": {args_str}}}\n```")
        }
        Fmt::Gemma4 => build_gemma4(name, args),
        Fmt::ActionTag => {
            format!(
                r#"<|action_start|>{{"name": "{name}", "arguments": {args_str}}}<|action_end|>"#
            )
        }
    }
}

/// Build Gemma 4 markup: `<|tool_call>call:NAME{<|"|>k<|"|>:<|"|>v<|"|>}<tool_call|>`.
fn build_gemma4(name: &str, args: &BTreeMap<String, String>) -> String {
    let mut body = String::from("{");
    for (i, (k, v)) in args.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        body.push_str(&format!("<|\"|>{k}<|\"|>:<|\"|>{v}<|\"|>"));
    }
    body.push('}');
    format!("<|tool_call>call:{name}{body}<tool_call|>")
}

/// Split a string into consecutive chunks of the given byte sizes, advancing to
/// the next char boundary so multi-byte markup is never split mid-character.
fn chunk_by_sizes(s: &str, sizes: &[usize]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut idx = 0;
    let n = s.len();
    for &sz in sizes {
        if idx >= n {
            break;
        }
        let mut end = (idx + sz.max(1)).min(n);
        while end < n && !s.is_char_boundary(end) {
            end += 1;
        }
        chunks.push(s[idx..end].to_string());
        idx = end;
    }
    if idx < n {
        chunks.push(s[idx..].to_string());
    }
    chunks
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Property 2: Preservation — tool-call parsing (Req 3.2)**
    ///
    /// *For any* well-formed markup in each of the six formats, `parse_text_tool_calls`
    /// SHALL CONTINUE TO produce a single `Part::FunctionCall` with the correct
    /// name and arguments. The markup carries no surrounding visible text, so the
    /// input never triggers the bug condition.
    ///
    /// **Validates: Requirements 3.2**
    #[test]
    fn prop_tool_call_parsing_preserved(
        fmt in arb_fmt(),
        name in arb_name(),
        args in arb_args(),
    ) {
        let markup = build_markup(fmt, &name, &args);
        let parts = parse_text_tool_calls(&markup);
        prop_assert!(parts.is_some(), "markup should parse: {markup:?}");
        let parts = parts.unwrap();
        prop_assert_eq!(parts.len(), 1, "expected exactly one part for {:?}: {:?}", markup, parts);
        match &parts[0] {
            Part::FunctionCall { name: n, args: a, .. } => {
                prop_assert_eq!(n, &name);
                prop_assert_eq!(a, &json_object(&args));
            }
            other => prop_assert!(false, "expected FunctionCall, got {other:?}"),
        }
    }

    /// **Property 2: Preservation — split-chunk buffering (Req 3.3)**
    ///
    /// *For any* Qwen tool-call markup split into arbitrary deltas, the
    /// `ToolCallBuffer` SHALL CONTINUE TO buffer across chunks and emit exactly
    /// one parsed `Part::FunctionCall`, with no stray text.
    ///
    /// **Validates: Requirements 3.3**
    #[test]
    fn prop_split_chunk_tool_call_preserved(
        name in arb_name(),
        args in arb_args(),
        sizes in prop::collection::vec(1usize..6, 1..30),
    ) {
        let markup = build_markup(Fmt::Qwen, &name, &args);
        let chunks = chunk_by_sizes(&markup, &sizes);

        let mut buf = ToolCallBuffer::new();
        let mut fcalls: Vec<(String, serde_json::Value)> = Vec::new();
        let mut text = String::new();

        let collect = |part: Part, fcalls: &mut Vec<(String, serde_json::Value)>, text: &mut String| {
            match part {
                Part::FunctionCall { name, args, .. } => fcalls.push((name, args)),
                Part::Text { text: t } => text.push_str(&t),
                _ => {}
            }
        };

        for c in &chunks {
            if let BufferAction::Emit(parts) = buf.push(c) {
                for p in parts {
                    collect(p, &mut fcalls, &mut text);
                }
            }
        }
        for p in buf.flush() {
            collect(p, &mut fcalls, &mut text);
        }

        prop_assert_eq!(fcalls.len(), 1, "expected one tool call from {:?}, stray text={:?}", markup, text);
        prop_assert_eq!(&fcalls[0].0, &name);
        prop_assert_eq!(&fcalls[0].1, &json_object(&args));
        prop_assert!(text.is_empty(), "no stray text expected, got {text:?}");
    }

    /// **Property 2: Preservation — overflow flush (Req 3.4)**
    ///
    /// *For any* buffered content that grows beyond `MAX_BUFFER_SIZE` (4096)
    /// without completing a tool call, the `ToolCallBuffer` SHALL CONTINUE TO
    /// flush the accumulated content as a single `Part::Text`.
    ///
    /// **Validates: Requirements 3.4**
    #[test]
    fn prop_overflow_flushes_as_text(
        extra in 1usize..1500,
        fill in prop::sample::select(vec!['a', 'b', 'X', '7', 'z']),
    ) {
        let mut buf = ToolCallBuffer::new();

        // Open a tool-call prefix so the buffer starts buffering.
        prop_assert!(matches!(buf.push("<tool_call>"), BufferAction::Buffering));

        // Push a large non-whitespace body with no closing tag to exceed the cap.
        let big: String = std::iter::repeat_n(fill, 4096 + extra).collect();
        let expected = format!("<tool_call>{big}");

        match buf.push(&big) {
            BufferAction::Emit(parts) => {
                prop_assert_eq!(parts.len(), 1);
                match &parts[0] {
                    Part::Text { text } => prop_assert_eq!(text, &expected),
                    other => prop_assert!(false, "expected Text, got {other:?}"),
                }
            }
            BufferAction::Buffering => prop_assert!(false, "expected overflow flush as text"),
        }
    }

    /// **Property 2: Preservation — name/args trimming (Req 3.5)**
    ///
    /// *For any* Qwen function-tag markup whose tool name and argument payload are
    /// padded with surrounding whitespace, the parser SHALL CONTINUE TO trim that
    /// whitespace from the extracted name and arguments. The padding lives inside
    /// the markup (not at a visible-text boundary), so it stays outside the bug
    /// condition.
    ///
    /// **Validates: Requirements 3.5**
    #[test]
    fn prop_name_and_args_trimmed(
        name in arb_name(),
        args in arb_args(),
        lpad in "[ \t]{0,4}",
        rpad in "[ \t]{0,4}",
        bpad_l in "[ \t]{0,4}",
        bpad_r in "[ \t]{0,4}",
    ) {
        let args_str = serde_json::to_string(&json_object(&args)).unwrap();
        let markup = format!(
            "<tool_call><function={lpad}{name}{rpad}>{bpad_l}{args_str}{bpad_r}</function></tool_call>"
        );
        let parts = parse_text_tool_calls(&markup).expect("function-tag markup should parse");
        prop_assert_eq!(parts.len(), 1);
        match &parts[0] {
            Part::FunctionCall { name: n, args: a, .. } => {
                prop_assert_eq!(n, &name);
                prop_assert_eq!(a, &json_object(&args));
            }
            other => prop_assert!(false, "expected FunctionCall, got {other:?}"),
        }
    }

    /// **Property 2: Preservation — non-whitespace text (Req 3.1)**
    ///
    /// *For any* sequence of non-whitespace text deltas with no tool-call markup,
    /// the `ToolCallBuffer` SHALL CONTINUE TO emit them as `Part::Text` such that
    /// the concatenation reproduces the input.
    ///
    /// **Validates: Requirements 3.1**
    #[test]
    fn prop_non_whitespace_text_emitted(
        frags in prop::collection::vec("[a-zA-Z0-9]{1,20}", 1..8),
    ) {
        let mut buf = ToolCallBuffer::new();
        let mut out = String::new();

        for f in &frags {
            if let BufferAction::Emit(parts) = buf.push(f) {
                for p in parts {
                    match p {
                        Part::Text { text } => out.push_str(&text),
                        Part::FunctionCall { .. } => prop_assert!(false, "unexpected tool call"),
                        _ => {}
                    }
                }
            }
        }
        for p in buf.flush() {
            match p {
                Part::Text { text } => out.push_str(&text),
                Part::FunctionCall { .. } => prop_assert!(false, "unexpected tool call"),
                _ => {}
            }
        }

        let expected: String = frags.concat();
        prop_assert_eq!(out, expected);
    }
}

/// **Property 2: Preservation — empty-buffer flush (Req 3.6)**
///
/// Flushing an empty buffer SHALL CONTINUE TO emit no parts. This is
/// deterministic, so it is expressed as a direct assertion rather than a
/// generated property.
///
/// **Validates: Requirements 3.6**
#[test]
fn empty_buffer_flush_emits_nothing() {
    let mut buf = ToolCallBuffer::new();
    assert!(buf.flush().is_empty());
}
