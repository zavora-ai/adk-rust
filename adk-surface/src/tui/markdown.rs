//! Agent messages as Markdown, wrapped to the pane, code highlighted (spec 3.3 §6.1 `tui`).
//!
//! Every piece of text goes through [`terminal_safe`](super::terminal_safe) before it becomes
//! a span (§11 S-I9).

use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

use super::safe::{safe_line, safe_lines};
use super::theme::{Appearance, Theme};

/// One line before wrapping: its spans, and the indent its continuation lines take.
#[derive(Debug, Clone, Default)]
pub(crate) struct Row {
    pub spans: Vec<Span<'static>>,
    pub indent: usize,
    /// Code: kept as is, cut at the pane's edge rather than wrapped.
    pub verbatim: bool,
}

impl Row {
    pub(crate) fn new(spans: Vec<Span<'static>>) -> Self {
        Self { spans, indent: 0, verbatim: false }
    }

    pub(crate) fn indent(mut self, indent: usize) -> Self {
        self.indent = indent;
        self
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

/// `text` as Markdown rows in `theme`, with each line prefixed by `prefix`.
pub(crate) fn markdown(text: &str, theme: &Theme) -> Vec<Row> {
    let mut out = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut styles: Vec<Style> = vec![theme.text()];
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote = 0usize;
    let mut code: Option<(String, String)> = None;
    let mut item_prefix: Option<String> = None;

    let flush = |out: &mut Vec<Row>,
                 current: &mut Vec<Span<'static>>,
                 lists: &Vec<Option<u64>>,
                 quote: usize,
                 item_prefix: &mut Option<String>| {
        if current.is_empty() && item_prefix.is_none() {
            return;
        }
        let mut spans = Vec::new();
        let depth = lists.len().saturating_sub(1);
        let mut indent = 0;
        if quote > 0 {
            spans.push(Span::styled("│ ".repeat(quote), theme.label()));
            indent += 2 * quote;
        }
        let pad = "  ".repeat(depth);
        indent += pad.len();
        match item_prefix.take() {
            Some(prefix) => {
                indent += prefix.width();
                spans.push(Span::raw(pad));
                spans.push(Span::styled(prefix, theme.label()));
            }
            None if !lists.is_empty() => {
                spans.push(Span::raw(format!("{pad}  ")));
                indent += 2;
            }
            None => {}
        }
        spans.append(current);
        out.push(Row::new(spans).indent(indent));
    };

    let parser = Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    let style = match level {
                        HeadingLevel::H1 | HeadingLevel::H2 => {
                            theme.text().add_modifier(Modifier::BOLD)
                        }
                        _ => theme.secondary().add_modifier(Modifier::BOLD),
                    };
                    styles.push(style);
                }
                Tag::BlockQuote(_) => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    let language = match kind {
                        CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").to_string()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    code = Some((language, String::new()));
                }
                Tag::List(start) => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    lists.push(start);
                }
                Tag::Item => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    let prefix = match lists.last_mut() {
                        Some(Some(number)) => {
                            let prefix = format!("{number}. ");
                            *number += 1;
                            prefix
                        }
                        _ => "• ".to_string(),
                    };
                    item_prefix = Some(prefix);
                }
                Tag::Emphasis => {
                    let top = *styles.last().unwrap_or(&theme.text());
                    styles.push(top.add_modifier(Modifier::ITALIC));
                }
                Tag::Strong => {
                    let top = *styles.last().unwrap_or(&theme.text());
                    styles.push(top.add_modifier(Modifier::BOLD));
                }
                Tag::Strikethrough => {
                    let top = *styles.last().unwrap_or(&theme.text());
                    styles.push(top.add_modifier(Modifier::CROSSED_OUT));
                }
                Tag::Link { .. } => {
                    let top = *styles.last().unwrap_or(&theme.text());
                    styles.push(top.patch(theme.accent()).add_modifier(Modifier::UNDERLINED));
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    if lists.is_empty() {
                        out.push(Row::default());
                    }
                }
                TagEnd::Heading(_) => {
                    styles.pop();
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                }
                TagEnd::BlockQuote(_) => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    quote = quote.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    if let Some((language, body)) = code.take() {
                        out.extend(code_rows(&language, &body, theme));
                        out.push(Row::default());
                    }
                }
                TagEnd::List(_) => {
                    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                    lists.pop();
                    if lists.is_empty() {
                        out.push(Row::default());
                    }
                }
                TagEnd::Item => flush(&mut out, &mut current, &lists, quote, &mut item_prefix),
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                    styles.pop();
                }
                _ => {}
            },
            Event::Text(text) => match &mut code {
                Some((_, body)) => body.push_str(&text),
                None => {
                    let style = *styles.last().unwrap_or(&theme.text());
                    current.push(Span::styled(safe_line(&text), style));
                }
            },
            Event::Code(text) => current.push(Span::styled(safe_line(&text), theme.accent())),
            Event::SoftBreak => current.push(Span::raw(" ")),
            Event::HardBreak => flush(&mut out, &mut current, &lists, quote, &mut item_prefix),
            Event::Rule => {
                flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
                out.push(Row::new(vec![Span::styled("───", theme.border())]));
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                current.push(Span::styled(safe_line(&text), theme.secondary()));
            }
            Event::TaskListMarker(done) => {
                current.push(Span::styled(if done { "[x] " } else { "[ ] " }, theme.label()));
            }
            _ => {}
        }
    }
    flush(&mut out, &mut current, &lists, quote, &mut item_prefix);
    while out.last().is_some_and(|row| row.spans.is_empty()) {
        out.pop();
    }
    out
}

/// A code block, highlighted when its language is known.
fn code_rows(language: &str, body: &str, theme: &Theme) -> Vec<Row> {
    let lines = safe_lines(body.trim_end_matches('\n'));
    let set = syntaxes();
    let syntax = (!language.is_empty())
        .then(|| set.find_syntax_by_token(language))
        .flatten()
        .unwrap_or_else(|| set.find_syntax_plain_text());
    let name = match theme.appearance() {
        Appearance::Light => "InspiredGitHub",
        Appearance::Dark => "base16-ocean.dark",
    };
    let mut highlighter = themes().themes.get(name).map(|t| HighlightLines::new(syntax, t));
    lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw("  ")];
            let with_newline = format!("{line}\n");
            let highlighted =
                highlighter.as_mut().and_then(|h| h.highlight_line(&with_newline, set).ok());
            match highlighted {
                Some(ranges) => {
                    for (style, text) in ranges {
                        let text = text.trim_end_matches('\n');
                        if text.is_empty() {
                            continue;
                        }
                        let color = (style.foreground.r, style.foreground.g, style.foreground.b);
                        spans.push(Span::styled(text.to_string(), theme.fg(color)));
                    }
                }
                None => spans.push(Span::styled(line, theme.secondary())),
            }
            Row { spans, indent: 2, verbatim: true }
        })
        .collect()
}

/// Rows wrapped to `width` cells.
pub(crate) fn wrap(rows: Vec<Row>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut out = Vec::new();
    for row in rows {
        if row.verbatim {
            let mut line = Vec::new();
            let mut used = 0;
            for span in row.spans {
                let mut text = String::new();
                for c in span.content.chars() {
                    let w = UnicodeWidthStr::width(c.encode_utf8(&mut [0; 4]) as &str);
                    if used + w > width {
                        break;
                    }
                    used += w;
                    text.push(c);
                }
                line.push(Span::styled(text, span.style));
            }
            out.push(Line::from(line));
            continue;
        }
        out.extend(wrap_spans(row.spans, width, row.indent));
    }
    out
}

/// Spans wrapped at spaces to `width` cells; continuation lines start `indent` cells in.
pub(crate) fn wrap_spans(
    spans: Vec<Span<'static>>,
    width: usize,
    indent: usize,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let indent = indent.min(width / 2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut line: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut start = 0usize;
    let mut space: Option<Style> = None;
    let break_line = |lines: &mut Vec<Line<'static>>,
                      line: &mut Vec<Span<'static>>,
                      used: &mut usize,
                      start: &mut usize| {
        trim_trailing_space(line);
        lines.push(Line::from(std::mem::take(line)));
        if indent > 0 {
            line.push(Span::raw(" ".repeat(indent)));
        }
        *used = indent;
        *start = indent;
    };
    for span in spans {
        let style = span.style;
        // Padding (an indent, a list's nesting) is kept as it is.
        if span.content.trim().is_empty() {
            let w = span.content.width();
            if used + w <= width {
                used += w;
                line.push(span);
            }
            continue;
        }
        for (index, word) in span.content.split(' ').enumerate() {
            if index > 0 {
                space = Some(style);
            }
            if word.is_empty() {
                continue;
            }
            let w = word.width();
            let gap = usize::from(space.is_some() && used > start);
            if used + gap + w <= width {
                if gap == 1 {
                    line.push(Span::styled(" ", space.unwrap_or(style)));
                }
                used += gap + w;
                line.push(Span::styled(word.to_string(), style));
                space = None;
                continue;
            }
            space = None;
            if used > start && w <= width - indent {
                break_line(&mut lines, &mut line, &mut used, &mut start);
                used += w;
                line.push(Span::styled(word.to_string(), style));
                continue;
            }
            // A word longer than a line: cut it.
            if gap == 1 && used + 1 < width {
                line.push(Span::styled(" ", style));
                used += 1;
            }
            let mut rest = word.to_string();
            loop {
                if used >= width {
                    break_line(&mut lines, &mut line, &mut used, &mut start);
                }
                let room = width - used;
                let mut head = String::new();
                let mut head_width = 0;
                let mut tail = String::new();
                for c in rest.chars() {
                    let cw = UnicodeWidthStr::width(c.encode_utf8(&mut [0; 4]) as &str);
                    if !tail.is_empty() || head_width + cw > room {
                        tail.push(c);
                    } else {
                        head_width += cw;
                        head.push(c);
                    }
                }
                if head.is_empty() {
                    // Not even one character fits beside the indent.
                    line.push(Span::styled(tail, style));
                    used = width;
                    break;
                }
                used += head_width;
                line.push(Span::styled(head, style));
                if tail.is_empty() {
                    break;
                }
                break_line(&mut lines, &mut line, &mut used, &mut start);
                rest = tail;
            }
        }
    }
    trim_trailing_space(&mut line);
    lines.push(Line::from(line));
    lines
}

fn trim_trailing_space(line: &mut Vec<Span<'static>>) {
    while line.last().is_some_and(|span| span.content == " ") {
        line.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn paragraphs_lists_and_code() {
        let theme = Theme::new(Appearance::Light, true);
        let rows = markdown("Hello **you**.\n\n- one\n- two\n\n```rust\nfn main() {}\n```", &theme);
        let lines = text(&wrap(rows, 40));
        assert_eq!(lines, vec!["Hello you.", "", "• one", "• two", "", "  fn main() {}"]);
    }

    #[test]
    fn long_text_wraps_at_spaces() {
        let theme = Theme::new(Appearance::Dark, true);
        let lines = text(&wrap(markdown("aaa bbb ccc ddd", &theme), 8));
        assert_eq!(lines, vec!["aaa bbb", "ccc ddd"]);
        let lines = text(&wrap(markdown("abcdefghijkl", &theme), 8));
        assert_eq!(lines, vec!["abcdefgh", "ijkl"]);
    }

    #[test]
    fn control_characters_are_escaped() {
        let theme = Theme::new(Appearance::Dark, true);
        let lines = text(&wrap(markdown("say \x1b[2Jhi", &theme), 80));
        assert_eq!(lines, vec!["say \\u{1b}[2Jhi"]);
    }
}
