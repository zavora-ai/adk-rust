//! Bug condition exploration test for whitespace loss in streaming text deltas.
//!
//! Spec: `preserve-whitespace-stream-deltas`
//!
//! **Property 1: Bug Condition — Byte-Exact Reconstruction of Visible Text**
//!
//! Streaming text deltas are routed through [`ToolCallBuffer`] so text-based
//! tool-call markup can be detected. The buffer's flush paths currently gate
//! emission on `text.trim().is_empty()`, so whitespace-only deltas (`"\n\n"`,
//! `" "`, `"\t"`) are silently dropped. Concatenating the emitted `Part::Text`
//! values therefore no longer reconstructs the original visible stream.
//!
//! This test MUST FAIL on the unfixed code — the failure confirms the bug.
//! It encodes the expected (correct) behavior and will pass once the fix lands.
//!
//! The bug is deterministic, so the property is scoped to the two concrete
//! failing cases from the design for reproducibility.
//!
//! **Validates: Requirements 2.1, 2.2, 2.3, 2.4**

use adk_core::Part;
use adk_model::tool_call_parser::{BufferAction, ToolCallBuffer};

/// Feed a sequence of stream deltas through `ToolCallBuffer::push`/`flush`,
/// collect all emitted `Part::Text` values, and concatenate them.
fn reconstruct_visible_text(deltas: &[&str]) -> String {
    let mut buffer = ToolCallBuffer::new();
    let mut text = String::new();

    for delta in deltas {
        if let BufferAction::Emit(parts) = buffer.push(delta) {
            for part in parts {
                if let Part::Text { text: t } = part {
                    text.push_str(&t);
                }
            }
        }
    }

    for part in buffer.flush() {
        if let Part::Text { text: t } = part {
            text.push_str(&t);
        }
    }

    text
}

/// Counterexample 1 (Markdown structure): whitespace-only deltas separating
/// visible text are dropped, collapsing the Markdown heading into the list.
///
/// Expected (fixed): `Heading\n\n- first\n- second`
/// Observed (unfixed): `Heading- first- second`
#[test]
fn bug_markdown_structure_whitespace_preserved() {
    let deltas = ["Heading", "\n\n", "- first", "\n", "- second"];
    let reconstructed = reconstruct_visible_text(&deltas);
    assert_eq!(
        reconstructed, "Heading\n\n- first\n- second",
        "whitespace-only deltas must be preserved for byte-exact reconstruction"
    );
}

/// Counterexample 2 (single-space separation): the whitespace-only `" "` delta
/// is dropped, collapsing two words together.
///
/// Expected (fixed): `Hello world`
/// Observed (unfixed): `Helloworld`
#[test]
fn bug_single_space_separation_preserved() {
    let deltas = ["Hello", " ", "world"];
    let reconstructed = reconstruct_visible_text(&deltas);
    assert_eq!(
        reconstructed, "Hello world",
        "single-space whitespace delta must be preserved for byte-exact reconstruction"
    );
}
