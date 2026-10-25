/**
 * Text a model or a tool wrote, made inert before a person reads it (spec 3.3 §11 S-I9). React
 * already escapes markup; this covers what the browser would still act on when it draws the
 * text: every control character other than a newline or a tab, and every character that
 * reorders or hides text — bidi controls, zero-width characters and the byte-order mark. The
 * terminal escapes the same characters (`adk_surface::tui::terminal_safe`), so a decision
 * reads the same on every surface.
 */

/** Every character the terminal escapes: `char::is_control` but `\n` and `\t`, bidi, zero-width. */
const UNSAFE = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u061c\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g;

/** `text` with each unsafe character written as its `\u{..}` escape, as the terminal writes it. */
export function visible(text: string): string {
  return text.replace(UNSAFE, (c) => `\\u{${c.charCodeAt(0).toString(16)}}`);
}

/**
 * A JSON text with each unsafe character written as a JSON `\uXXXX` escape. Such characters
 * occur only inside strings, so the text still parses to the same value.
 */
export function visibleJson(json: string): string {
  return json.replace(UNSAFE, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`);
}
