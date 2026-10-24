//! Escaping model, tool and file text before the terminal shows it (spec 3.3 §11 S-I9).

use std::borrow::Cow;
use std::fmt::Write;

/// Whether `c` must not reach the terminal as itself: a control character other than a
/// newline or a tab, or a character that reorders or hides text (bidi controls, zero-width
/// characters, the byte-order mark).
fn unsafe_char(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(
            c,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

/// Escapes terminal control characters in model-, tool- or file-controlled text (spec 3.3
/// §11 S-I9).
///
/// Newlines and tabs are kept. Every other control character — the `ESC` that starts an ANSI
/// or OSC sequence, a C1 `CSI`, a carriage return — and every bidi or zero-width character is
/// written as its `\u{..}` escape, so text a model or a tool wrote cannot move the cursor,
/// rewrite the screen or the window title, set the clipboard, or reorder what a person reads.
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::terminal_safe;
///
/// assert_eq!(terminal_safe("ok\n"), "ok\n");
/// assert_eq!(terminal_safe("\x1b]0;pwned\x07"), "\\u{1b}]0;pwned\\u{7}");
/// assert_eq!(terminal_safe("a\u{202e}b"), "a\\u{202e}b");
/// ```
pub fn terminal_safe(text: &str) -> Cow<'_, str> {
    if !text.chars().any(unsafe_char) {
        return Cow::Borrowed(text);
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if unsafe_char(c) {
            escaped.extend(c.escape_unicode());
        } else {
            escaped.push(c);
        }
    }
    Cow::Owned(escaped)
}

/// A JSON text with every character [`terminal_safe`] escapes written as a JSON `\uXXXX`
/// escape, so a JSON line printed on a terminal is as inert as the text UI (spec 3.3 §11
/// S-I9). The JSON encoder already escapes C0 controls; this covers `DEL`, the C1 controls
/// and the bidi and zero-width characters, which occur only inside strings, so the value is
/// unchanged.
pub(crate) fn json_safe(json: &str) -> Cow<'_, str> {
    if !json.chars().any(unsafe_char) {
        return Cow::Borrowed(json);
    }
    let mut escaped = String::with_capacity(json.len() + 12);
    for c in json.chars() {
        if unsafe_char(c) {
            // Every such character is in the Basic Multilingual Plane: one escape each.
            let _ = write!(escaped, "\\u{:04x}", u32::from(c));
        } else {
            escaped.push(c);
        }
    }
    Cow::Owned(escaped)
}

/// `text` escaped, split into lines, with tabs as spaces: what one cell row can show.
pub(crate) fn safe_lines(text: &str) -> Vec<String> {
    terminal_safe(text).split('\n').map(|line| line.replace('\t', "    ")).collect()
}

/// `text` escaped, on one line: newlines and tabs as spaces.
pub(crate) fn safe_line(text: &str) -> String {
    terminal_safe(text).replace(['\n', '\t'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_escaped_and_layout_is_kept() {
        assert_eq!(terminal_safe("plain"), Cow::Borrowed("plain"));
        assert_eq!(terminal_safe("a\tb\nc"), "a\tb\nc");
        assert_eq!(terminal_safe("x\ry"), "x\\u{d}y");
        assert_eq!(terminal_safe("\u{9b}31m"), "\\u{9b}31m");
        assert_eq!(terminal_safe("\u{feff}"), "\\u{feff}");
        assert_eq!(safe_lines("a\tb\nc"), vec!["a    b".to_string(), "c".to_string()]);
        assert_eq!(safe_line("a\nb\tc"), "a b c");
        let json = serde_json::to_string("\u{7f}\u{9b}\u{202e}x").unwrap();
        let safe = json_safe(&json);
        assert_eq!(safe, r#""\u007f\u009b\u202ex""#);
        assert_eq!(serde_json::from_str::<String>(&safe).unwrap(), "\u{7f}\u{9b}\u{202e}x");
    }
}
