//! Byte-exact reconstruction property test for the whitespace-in-stream-deltas
//! bugfix.
//!
//! Spec: `preserve-whitespace-stream-deltas`
//!
//! **Property 1: Bug Condition — Byte-Exact Reconstruction of Visible Text**
//!
//! Streaming text deltas are routed through [`ToolCallBuffer`] so text-based
//! tool-call markup can be detected and converted into `Part::FunctionCall`.
//! The correct behavior after the fix is that the concatenation of every
//! emitted `Part::Text` reproduces the *visible* (non-tool-call) source text
//! byte for byte and in original order — regardless of how the source is split
//! into deltas, and regardless of whitespace-only deltas or whitespace that
//! abuts tool-call markup.
//!
//! This test generates arbitrary interleavings of visible text fragments
//! (including whitespace-only fragments such as `"\n\n"`, `" "`, `"\t"`) with a
//! well-formed tool-call markup, splits the combined source into arbitrary
//! deltas, feeds them through `ToolCallBuffer::push`/`flush`, and asserts that
//! the concatenation of emitted `Part::Text` equals the visible source text.
//!
//! Generator design notes (per the design's "be careful about markup that
//! emits its own surrounding text"):
//!
//! - Visible fragments never contain any character that can begin a tool-call
//!   prefix (`<`, `[`, `` ` ``, or the full-width `｜`), so they can never
//!   accidentally form markup, and they never trigger a false partial-prefix.
//! - Each stream carries at most one markup so the byte-exact-reconstruction
//!   property isolates whitespace handling from the parser's pre-existing
//!   multi-markup recovery behavior. The markup uses one of the balanced
//!   open/close formats (Qwen, Gemma 4, action tags) that emit both the
//!   leading (`before`) and trailing visible slices, so no visible byte
//!   adjacent to the markup can be dropped.
//! - The tool name and structured argument payload inside the markup contain no
//!   whitespace, so the markup's own bytes are never part of the visible text.
//!
//! **Validates: Requirements 2.6**

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

/// A whitespace-only visible fragment — the core trigger of the bug.
fn arb_ws_fragment() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "\n", "\n\n", " ", "  ", "\t", "\t\t", " \n", "\n ", "\t\n", "   ", " \n ",
    ])
    .prop_map(|s| s.to_string())
}

/// A non-whitespace visible fragment. Deliberately excludes `<`, `[`, `` ` ``,
/// and the full-width `｜` so it can never form (or partially form) a tool-call
/// prefix.
fn arb_text_fragment() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9.,!?()_+=@#%^&*-]{1,12}".prop_map(|s| s.to_string())
}

/// A single visible fragment: either whitespace-only or a non-whitespace token.
fn arb_fragment() -> impl Strategy<Value = String> {
    prop_oneof![arb_ws_fragment(), arb_text_fragment()]
}

/// A sequence of visible fragments (may be empty).
fn arb_fragments() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(arb_fragment(), 0..8)
}

/// Build the args JSON object (string-valued) for well-formed markup.
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

/// Feed the deltas through the buffer (push each, then flush) and return the
/// concatenation of all emitted `Part::Text` values.
fn reconstruct(deltas: &[String]) -> String {
    let mut buf = ToolCallBuffer::new();
    let mut out = String::new();
    let collect = |parts: Vec<Part>, out: &mut String| {
        for part in parts {
            if let Part::Text { text } = part {
                out.push_str(&text);
            }
        }
    };
    for delta in deltas {
        if let BufferAction::Emit(parts) = buf.push(delta) {
            collect(parts, &mut out);
        }
    }
    collect(buf.flush(), &mut out);
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Property 1: Byte-Exact Reconstruction of Visible Text**
    ///
    /// *For any* interleaving of visible text fragments (including
    /// whitespace-only fragments) with at most one well-formed tool-call markup,
    /// split into *arbitrary* deltas, the concatenation of every emitted
    /// `Part::Text` SHALL equal the visible (non-tool-call) source text, byte for
    /// byte and in original order.
    ///
    /// **Validates: Requirements 2.6**
    #[test]
    fn prop_byte_exact_reconstruction(
        pre in arb_fragments(),
        markup in prop::option::of((arb_fmt(), arb_name(), arb_args())),
        post in arb_fragments(),
        sizes in prop::collection::vec(1usize..7, 1..60),
    ) {
        // The visible source text is everything that is NOT tool-call markup:
        // the fragments before and after the markup, concatenated in order.
        let mut visible = String::new();
        visible.extend(pre.iter().cloned());
        visible.extend(post.iter().cloned());

        // The full source stream interleaves the visible fragments with the
        // markup (which contributes no visible bytes).
        let mut source = String::new();
        source.extend(pre.iter().cloned());
        if let Some((fmt, name, args)) = &markup {
            source.push_str(&build_markup(*fmt, name, args));
        }
        source.extend(post.iter().cloned());

        let deltas = chunk_by_sizes(&source, &sizes);
        let reconstructed = reconstruct(&deltas);

        prop_assert_eq!(
            &reconstructed,
            &visible,
            "visible text not reconstructed byte-exactly\n  source={:?}\n  deltas={:?}",
            source,
            deltas
        );
    }
}
