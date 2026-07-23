//! Preservation equivalence property test for the whitespace-in-stream-deltas
//! bugfix.
//!
//! Spec: `preserve-whitespace-stream-deltas`
//!
//! **Property 2: Preservation — Unchanged Behavior for Non-Buggy Streams**
//!
//! Formal statement:
//!
//! ```text
//! FOR ALL stream WHERE NOT isBugCondition(stream): F(stream) = F'(stream)
//! ```
//!
//! The fix (in `adk-model/src/tool_call_parser.rs`) only changes behavior for
//! streams that trigger the bug condition: a whitespace-only delta, or
//! whitespace that abuts tool-call markup. For every other stream the fixed
//! buffer/parser (`F'`) must produce exactly the same sequence of `Part`s as the
//! original (`F`) — same `Part::Text` runs and same `Part::FunctionCall`s, in the
//! same order.
//!
//! We cannot run the pre-fix code `F` side by side, so (following the
//! observation-first approach used by the Task 2 preservation tests) we generate
//! streams that provably sit OUTSIDE the bug condition and assert the fixed
//! emission sequence matches the known-correct expected sequence. For these
//! inputs the expected sequence equals the original behavior, so a match
//! establishes `F(stream) = F'(stream)`.
//!
//! Generator design (keeping every input strictly outside the bug condition):
//!
//! - Visible text fragments are non-whitespace and never contain a character
//!   that can begin a tool-call prefix (`<`, `[`, `` ` ``, `|`, or the full-width
//!   `｜`), so they can never form (or partially form) markup and never sit as
//!   whitespace at a tool-call boundary.
//! - Markup uses only the balanced open/close formats (Qwen, Gemma 4, action
//!   tags) that emit BOTH the leading (`before`) and trailing visible slices, so
//!   no visible byte adjacent to markup is ever dropped.
//! - Consecutive tool calls are always separated by a non-whitespace text run of
//!   at least 8 characters. With deltas capped at 6 bytes, a completed markup's
//!   buffer can never swallow the prefix of the following markup, so each call is
//!   parsed independently and deterministically.
//! - Tool names and argument payloads contain no whitespace, so the markup's own
//!   bytes are never part of the visible text and name/args trimming is a no-op.
//!
//! The whole emission sequence is compared (both `Part::Text` and
//! `Part::FunctionCall`, in order) after collapsing consecutive `Part::Text`
//! runs into one — the canonical form both `F` and `F'` agree on, since neither
//! drops nor reorders visible bytes and neither alters the parsed calls.
//!
//! **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6**

use std::collections::BTreeMap;

use adk_core::Part;
use adk_model::tool_call_parser::{BufferAction, ToolCallBuffer};
use proptest::prelude::*;

/// The balanced-tag markup formats used by this test. Each emits both the
/// leading and trailing visible text slices surrounding the markup, so visible
/// bytes adjacent to the markup are preserved.
#[derive(Debug, Clone, Copy)]
enum Fmt {
    Qwen,
    Gemma4,
    ActionTag,
}

fn arb_fmt() -> impl Strategy<Value = Fmt> {
    prop_oneof![Just(Fmt::Qwen), Just(Fmt::Gemma4), Just(Fmt::ActionTag)]
}

/// A tool-call function name (identifier, no whitespace).
fn arb_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{1,15}".prop_map(|s| s.to_string())
}

/// Argument keys/values are non-whitespace tokens so the markup stays
/// well-formed and its bytes never leak into the visible text.
fn arb_args() -> impl Strategy<Value = BTreeMap<String, String>> {
    prop::collection::btree_map("[a-z][a-z0-9_]{1,10}", "[a-zA-Z0-9]{1,12}", 0..3)
}

/// A non-whitespace visible text token. Deliberately excludes `<`, `[`, `` ` ``,
/// `|`, and the full-width `｜` so it can never form (or partially form) a
/// tool-call prefix or a closing tag, and never sits as boundary whitespace.
fn arb_text_token() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9.,!?()_+=@#%^&*-]{1,12}".prop_map(|s| s.to_string())
}

/// A separator text run placed between two tool calls. At least 8 characters so
/// that (with deltas capped at 6 bytes) a completed markup's buffer can never
/// swallow the following markup's prefix.
fn arb_separator() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9.,!?()_+=@#%^&*-]{8,16}".prop_map(|s| s.to_string())
}

/// A single stream segment: either visible text or a tool call.
#[derive(Debug, Clone)]
enum Seg {
    Text(String),
    Call { fmt: Fmt, name: String, args: BTreeMap<String, String> },
}

/// Build the expected `args` JSON object (string-valued) for comparison.
fn json_object(args: &BTreeMap<String, String>) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> =
        args.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    serde_json::Value::Object(map)
}

/// Build well-formed markup for a balanced-tag format.
fn build_markup(fmt: Fmt, name: &str, args: &BTreeMap<String, String>) -> String {
    let args_str = serde_json::to_string(&json_object(args)).unwrap();
    match fmt {
        Fmt::Qwen => {
            format!(r#"<tool_call>{{"name": "{name}", "arguments": {args_str}}}</tool_call>"#)
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

/// Render a segment list into the full source stream.
fn build_source(segs: &[Seg]) -> String {
    let mut source = String::new();
    for seg in segs {
        match seg {
            Seg::Text(t) => source.push_str(t),
            Seg::Call { fmt, name, args } => source.push_str(&build_markup(*fmt, name, args)),
        }
    }
    source
}

/// Build the expected canonical emission sequence: consecutive text runs are
/// merged into a single `Part::Text`, each tool call becomes a
/// `Part::FunctionCall` with the trimmed name and parsed arguments.
fn expected_parts(segs: &[Seg]) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::new();
    let mut acc = String::new();
    for seg in segs {
        match seg {
            Seg::Text(t) => acc.push_str(t),
            Seg::Call { fmt: _, name, args } => {
                if !acc.is_empty() {
                    out.push(Part::Text { text: std::mem::take(&mut acc) });
                }
                out.push(Part::FunctionCall {
                    name: name.clone(),
                    args: json_object(args),
                    id: None,
                    thought_signature: None,
                });
            }
        }
    }
    if !acc.is_empty() {
        out.push(Part::Text { text: acc });
    }
    out
}

/// Split a string into consecutive chunks, cycling through the given byte sizes
/// until the whole string is consumed. Cycling (rather than dumping the tail as
/// one large chunk) keeps every delta bounded by the maximum size, so a
/// completed markup's buffer can never accumulate a following markup — each tool
/// call is parsed independently. Advances to the next char boundary so
/// multi-byte markup is never split mid-character.
fn chunk_by_sizes(s: &str, sizes: &[usize]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut idx = 0;
    let n = s.len();
    let mut i = 0;
    while idx < n {
        let sz = sizes[i % sizes.len()].max(1);
        i += 1;
        let mut end = (idx + sz).min(n);
        while end < n && !s.is_char_boundary(end) {
            end += 1;
        }
        chunks.push(s[idx..end].to_string());
        idx = end;
    }
    chunks
}

/// Feed the deltas through the buffer (push each, then flush) and collect every
/// emitted `Part` in order.
fn emit_all(deltas: &[String]) -> Vec<Part> {
    let mut buf = ToolCallBuffer::new();
    let mut out: Vec<Part> = Vec::new();
    for delta in deltas {
        if let BufferAction::Emit(parts) = buf.push(delta) {
            out.extend(parts);
        }
    }
    out.extend(buf.flush());
    out
}

/// Collapse consecutive `Part::Text` runs into one — the canonical form used for
/// whole-sequence comparison. Non-text parts pass through unchanged.
fn normalize(parts: Vec<Part>) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => {
                if let Some(Part::Text { text: last }) = out.last_mut() {
                    last.push_str(&text);
                } else {
                    out.push(Part::Text { text });
                }
            }
            other => out.push(other),
        }
    }
    // Drop any empty text run (the buffer never emits one, but keep the
    // canonical form robust).
    out.retain(|p| !matches!(p, Part::Text { text } if text.is_empty()));
    out
}

/// Generate a segment list that never triggers the bug condition: optional
/// leading text, a run of balanced tool calls each separated by a non-empty
/// non-whitespace run, and optional trailing text.
fn arb_segments() -> impl Strategy<Value = Vec<Seg>> {
    prop::collection::vec((arb_fmt(), arb_name(), arb_args()), 0..4)
        .prop_flat_map(|calls| {
            let n_sep = calls.len().saturating_sub(1);
            (
                Just(calls),
                prop::collection::vec(arb_separator(), n_sep..=n_sep),
                prop::option::of(arb_text_token()),
                prop::option::of(arb_text_token()),
            )
        })
        .prop_map(|(calls, seps, leading, trailing)| {
            let mut segs = Vec::new();
            if let Some(t) = leading {
                segs.push(Seg::Text(t));
            }
            for (i, (fmt, name, args)) in calls.iter().enumerate() {
                segs.push(Seg::Call { fmt: *fmt, name: name.clone(), args: args.clone() });
                if i + 1 < calls.len() {
                    segs.push(Seg::Text(seps[i].clone()));
                }
            }
            if let Some(t) = trailing {
                segs.push(Seg::Text(t));
            }
            segs
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Property 2: Preservation — F(stream) = F'(stream) for non-buggy streams**
    ///
    /// *For any* stream that does NOT trigger the bug condition (no
    /// whitespace-only deltas, no whitespace at a tool-call boundary) — an
    /// arbitrary interleaving of non-whitespace text fragments and well-formed
    /// balanced tool-call markup, split into arbitrary deltas — the fixed
    /// buffer/parser SHALL emit the same `Part` sequence (both `Part::Text` runs
    /// and `Part::FunctionCall`s, in order) as the known-correct expected output,
    /// which equals the original pre-fix behavior for these inputs.
    ///
    /// **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6**
    #[test]
    fn prop_non_buggy_stream_emission_unchanged(
        segs in arb_segments(),
        sizes in prop::collection::vec(1usize..7, 1..60),
    ) {
        let source = build_source(&segs);
        let deltas = chunk_by_sizes(&source, &sizes);

        let actual = normalize(emit_all(&deltas));
        let expected = expected_parts(&segs);

        prop_assert_eq!(
            &actual,
            &expected,
            "emission sequence differs for non-buggy stream\n  source={:?}\n  deltas={:?}",
            source,
            deltas
        );
    }
}
