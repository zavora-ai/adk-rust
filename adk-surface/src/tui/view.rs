//! Drawing the terminal (spec 3.3 §5.2, §7.1, §9): the full-screen layout, and the lines the
//! inline UI prints. Every on-screen word comes from the engine's projection (§8); every string
//! a model, a tool or a file wrote passes through [`terminal_safe`](super::terminal_safe).

use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

use chrono::{Local, Timelike};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::app::{App, Input, OpenCard, Tone, View};
use super::markdown::{markdown, wrap, wrap_spans};
use super::safe::{safe_line, safe_lines};
use super::theme::Theme;
use crate::model::{AgentStatus, FeedEntry, Mode, Pill, Speaker, ThreadItem};
use crate::rules::Answer;
use crate::words;

/// How many argument rows a decision shows before `?` must show the whole call.
const COMPACT_ROWS: usize = 10;

/// How long a call's arguments may be, as JSON, before `?` must show the whole call.
const COMPACT_CHARS: usize = 600;

/// The widest a decision box is drawn.
const BOX_WIDTH: usize = 110;

/// The words of the §7.1 "Details" part, on the `?` key.
const DETAILS: &str = "Details";

/// Whether a decision's call is too long for the box: it must be shown in full with `?`
/// before it can be approved (spec 3.3 §7.1).
pub(crate) fn needs_details(open: &OpenCard) -> bool {
    let call = &open.card.call;
    call.truncated
        || call.args.to_string().chars().count() > COMPACT_CHARS
        || call.args.as_object().is_some_and(|map| map.len() > COMPACT_ROWS)
}

/// The key that gives `answer` (spec 3.3 §5.2).
pub(crate) fn key_of(answer: Answer) -> char {
    match answer {
        Answer::AllowOnce => 'y',
        Answer::AlwaysAllow => 'a',
        Answer::Edit => 'e',
        Answer::Deny => 'n',
    }
}

/// Draws the terminal, caching what is costly to lay out.
pub(crate) struct Renderer {
    pub theme: Theme,
    markdown: HashMap<(u64, usize, usize), Vec<Line<'static>>>,
}

impl Renderer {
    pub(crate) fn new(theme: Theme) -> Self {
        Self { theme, markdown: HashMap::new() }
    }

    // -----------------------------------------------------------------------
    // The status line and the footer
    // -----------------------------------------------------------------------

    pub(crate) fn status_line(&self, app: &App) -> Line<'static> {
        let t = &self.theme;
        let mut spans = vec![Span::styled(
            format!(" {} ", app.app_name),
            t.text().add_modifier(Modifier::BOLD),
        )];
        let part = |spans: &mut Vec<Span<'static>>, text: String, style: Style| {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(text, style));
        };
        if let Some(place) = &app.place {
            part(&mut spans, place.clone(), t.label());
        }
        if let Some(agent) = app.shown_agent() {
            part(&mut spans, safe_line(&agent.agent.display_name), t.text());
        }
        if let Some(status) = &app.status {
            if let Some(mode) = &status.mode {
                part(&mut spans, mode.label.clone(), t.text());
            }
            if let Some(model) = &app.model {
                part(&mut spans, safe_line(model), t.secondary());
            }
            if let Some(spend) = &status.spend {
                part(&mut spans, spend.text.clone(), t.text());
            }
            if let Some(quiet) = &status.quiet {
                part(&mut spans, quiet.text.clone(), t.label());
            }
        } else if let Some(model) = &app.model {
            part(&mut spans, safe_line(model), t.secondary());
        }
        // Never fewer than the decisions on screen, which the status line may not count yet.
        let counted = app.status.as_ref().map_or(0, |status| status.decisions as usize);
        let decisions = counted.max(app.cards.len());
        spans.push(Span::raw("  "));
        spans.push(Span::styled(words::DECISIONS, t.accent().add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!(" {decisions} "), t.badge()));
        if let Some(stopped) = app.status.as_ref().and_then(|status| status.stopped_text.as_ref()) {
            part(
                &mut spans,
                stopped.clone(),
                t.fg(t.palette().failed).add_modifier(Modifier::BOLD),
            );
        }
        Line::from(spans)
    }

    pub(crate) fn footer(&self) -> Line<'static> {
        let t = &self.theme;
        let hints = [
            ("tab", "agents"),
            ("ctrl-t", "today"),
            ("ctrl-r", "permissions"),
            ("/", "commands"),
            ("ctrl-k", "stop all"),
            ("esc", "never decides"),
        ];
        let mut spans = vec![Span::raw(" ")];
        for (key, what) in hints {
            spans.push(Span::styled(key, t.text().add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(format!(" {what}   "), t.label()));
        }
        Line::from(spans)
    }

    // -----------------------------------------------------------------------
    // The left column
    // -----------------------------------------------------------------------

    fn status_style(&self, status: AgentStatus) -> (char, Style) {
        let t = &self.theme;
        let p = t.palette();
        match status {
            AgentStatus::Working => ('●', t.fg(p.working)),
            AgentStatus::WaitingForYou => ('◆', t.accent()),
            AgentStatus::Asleep => ('○', t.label()),
            AgentStatus::Done => ('✓', t.fg(p.done)),
            AgentStatus::Failed => ('✗', t.fg(p.failed)),
        }
    }

    pub(crate) fn left(&self, app: &App, width: usize) -> Vec<Line<'static>> {
        let t = &self.theme;
        let inner = width.saturating_sub(2);
        let mut lines = vec![Line::default()];
        lines.push(Line::from(vec![Span::raw(" "), Span::styled("Agents", t.heading())]));
        for row in &app.agents {
            let (glyph, style) = self.status_style(row.status);
            let selected = row.session_id.as_deref() == Some(app.session.as_str());
            let name = safe_line(&row.agent.display_name);
            let status = row.status_text.clone();
            let room = inner.saturating_sub(status.width() + 3);
            let name = fit(&name, room);
            let gap = inner.saturating_sub(2 + name.width() + status.width());
            let name_style = if selected { t.accent() } else { t.text() };
            let mut line = Line::from(vec![
                Span::raw(" "),
                Span::styled(glyph.to_string(), style),
                Span::raw(" "),
                Span::styled(name, name_style),
                Span::raw(" ".repeat(gap)),
                Span::styled(status, style),
                Span::raw(" "),
            ]);
            if selected {
                line = line.style(Style::new().bg(t.color(t.palette().decision_strong)));
            }
            lines.push(line);
        }
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::raw(" "), Span::styled("Coming up", t.heading())]));
        let mut upcoming: Vec<_> =
            app.agents.iter().filter_map(|row| row.wake_at.map(|at| (at, row))).collect();
        upcoming.sort_by_key(|(at, _)| *at);
        if upcoming.is_empty() {
            lines.push(Line::from(vec![Span::raw(" "), Span::styled("—", t.label())]));
        }
        for (at, row) in upcoming {
            let local = at.with_timezone(&Local);
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{:02}:{:02}", local.hour(), local.minute()), t.text()),
                Span::raw(" "),
                Span::styled(
                    fit(&safe_line(&row.agent.display_name), inner.saturating_sub(7)),
                    t.text(),
                ),
            ]));
        }
        lines.push(Line::default());
        let title = app
            .briefing
            .as_ref()
            .map_or(words::WHILE_YOU_WERE_AWAY.to_string(), |briefing| briefing.title.clone());
        lines.push(Line::from(vec![Span::raw(" "), Span::styled(title, t.heading())]));
        if let Some(briefing) = &app.briefing {
            let mut counts: Vec<(Pill, String, usize)> = Vec::new();
            for line in &briefing.lines {
                if let (Some(pill), Some(text)) = (line.pill, &line.pill_text) {
                    match counts.iter_mut().find(|(p, _, _)| *p == pill) {
                        Some(entry) => entry.2 += 1,
                        None => counts.push((pill, text.clone(), 1)),
                    }
                }
            }
            let mut parts: Vec<Span<'static>> = Vec::new();
            for (pill, text, count) in counts {
                if !parts.is_empty() {
                    parts.push(Span::styled(" · ", t.label()));
                }
                parts.push(Span::styled(format!("{text} {count}"), self.pill_style(pill)));
            }
            if !parts.is_empty() {
                let mut spans = vec![Span::raw(" ")];
                spans.extend(parts);
                lines.extend(wrap_spans(spans, inner, 1));
            }
            let mut tail = vec![Span::raw(" ")];
            if let Some(spend) = &briefing.spend {
                tail.push(Span::styled(spend.text.clone(), t.text()));
            } else {
                tail.push(Span::styled(briefing.since_text.clone(), t.text()));
            }
            lines.push(Line::from(tail));
        }
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled("ctrl-t", t.text()),
            Span::styled(" to open Today", t.label()),
        ]));
        lines
    }

    fn pill_style(&self, pill: Pill) -> Style {
        let t = &self.theme;
        let p = t.palette();
        match pill {
            Pill::Done => t.fg(p.done),
            Pill::Failed => t.fg(p.failed),
            Pill::YouDecided => t.fg(p.working),
            Pill::WaitingForYou | Pill::Suggestion | Pill::Question => t.accent(),
        }
    }

    // -----------------------------------------------------------------------
    // The thread
    // -----------------------------------------------------------------------

    /// The lines of one feed entry. `results` are the later results by call, so an activity
    /// shows how it ended; without them (inline), a result prints its own line.
    pub(crate) fn entry_lines(
        &mut self,
        entry: &FeedEntry,
        results: Option<&HashMap<String, (bool, String)>>,
        width: usize,
    ) -> Vec<Line<'static>> {
        let t = self.theme;
        let mut lines = Vec::new();
        for (index, item) in entry.items.iter().enumerate() {
            match item {
                ThreadItem::Message { from, author, text } => {
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        format!("{} · {}", safe_line(author), entry.at_text),
                        t.label(),
                    )));
                    match from {
                        Speaker::You => {
                            for line in safe_lines(text) {
                                lines.extend(wrap_spans(
                                    vec![Span::styled(line, t.secondary())],
                                    width,
                                    0,
                                ));
                            }
                        }
                        Speaker::Agent => {
                            let key = (entry.seq, index, width);
                            let rendered = self
                                .markdown
                                .entry(key)
                                .or_insert_with(|| wrap(markdown(text, &t), width))
                                .clone();
                            lines.extend(rendered);
                        }
                    }
                }
                ThreadItem::Activity { text, call_id, .. } => {
                    let ended = results.and_then(|results| {
                        call_id.as_ref().and_then(|id| results.get(id)).cloned()
                    });
                    let (mark, mark_style, right) = match ended {
                        Some((true, text)) => ('✓', t.fg(t.palette().working), text),
                        Some((false, text)) => ('✗', t.fg(t.palette().failed), text),
                        None => ('·', t.label(), String::new()),
                    };
                    let left = fit(&safe_line(text), width.saturating_sub(right.width() + 4));
                    let gap = width.saturating_sub(2 + left.width() + right.width());
                    lines.push(Line::from(vec![
                        Span::styled(mark.to_string(), mark_style),
                        Span::raw(" "),
                        Span::styled(left, t.secondary()),
                        Span::raw(" ".repeat(gap)),
                        Span::styled(right, t.label()),
                    ]));
                }
                ThreadItem::Result { tool, call_id, ok, text } => {
                    let shown_on_activity = results.is_some() && call_id.is_some();
                    if !shown_on_activity {
                        let (mark, style) = if *ok {
                            ('✓', t.fg(t.palette().working))
                        } else {
                            ('✗', t.fg(t.palette().failed))
                        };
                        lines.push(Line::from(vec![
                            Span::styled(mark.to_string(), style),
                            Span::raw(" "),
                            Span::styled(fit(&safe_line(tool), width / 2), t.secondary()),
                            Span::styled(format!(" · {text}"), t.label()),
                        ]));
                    }
                }
                // A decision is drawn only as its box, from its open inbox entry (S-I1): the
                // event that held the call says "Waiting for you" also once it is decided.
                ThreadItem::Decision { .. } => {}
            }
        }
        lines
    }

    /// The thread of the shown session with its open decisions and notices, and the lines of
    /// the box the keys answer. Only that box is drawn whole; the other open decisions wait
    /// below it as one line each, so the box the keys answer is the one on screen (§7.1).
    pub(crate) fn thread(
        &mut self,
        app: &App,
        width: usize,
        now: Instant,
    ) -> (Vec<Line<'static>>, Option<Range<usize>>) {
        let mut results = HashMap::new();
        for entry in &app.entries {
            for item in &entry.items {
                if let ThreadItem::Result { call_id: Some(id), ok, text, .. } = item {
                    results.insert(id.clone(), (*ok, text.clone()));
                }
            }
        }
        let mut lines = Vec::new();
        for entry in &app.entries {
            lines.extend(self.entry_lines(entry, Some(&results), width));
        }
        let live = app.keys_answer(now);
        let mut focused = None;
        if let Some(open) = app.cards.first() {
            lines.push(Line::default());
            let hint = app.typing || !app.composer.is_empty();
            let start = lines.len();
            lines.extend(self.decision_box(open, width, live, hint));
            focused = Some(start..lines.len());
            lines.extend(self.also_shown(open, width));
        }
        let t = self.theme;
        for open in app.cards.iter().skip(1) {
            let text = format!("{} · {}", open.card.status_text, safe_line(&open.card.heading));
            lines.push(Line::from(vec![
                Span::styled("◆ ", t.accent()),
                Span::styled(fit(&text, width.saturating_sub(16)), t.secondary()),
                Span::styled(" · after this one", t.label()),
            ]));
        }
        if !app.notices.is_empty() {
            lines.push(Line::default());
        }
        for notice in &app.notices {
            lines.extend(self.notice(&notice.text, notice.tone, width));
        }
        (lines, focused)
    }

    pub(crate) fn also_shown(&self, open: &OpenCard, width: usize) -> Vec<Line<'static>> {
        let text = match &open.card.also_shown {
            Some(also) => format!("{also}. {}", words::ANSWERED_ELSEWHERE_UPDATES),
            None => words::ANSWERED_ELSEWHERE_UPDATES.to_string(),
        };
        wrap_spans(vec![Span::styled(text, self.theme.label())], width, 0)
    }

    pub(crate) fn notice(&self, text: &str, tone: Tone, width: usize) -> Vec<Line<'static>> {
        let t = &self.theme;
        let (mark, style) = match tone {
            Tone::Decided => ("✓ ", t.fg(t.palette().working)),
            Tone::Problem => ("! ", t.accent()),
            Tone::Info => ("· ", t.label()),
        };
        wrap_spans(
            vec![Span::styled(mark, style), Span::styled(safe_line(text), t.secondary())],
            width,
            2,
        )
    }

    /// A decision as a box answered by key (spec 3.3 §5.2, §7.1): who and when, what will
    /// happen, the exact call, how it got here, the answers this terminal may give, details.
    pub(crate) fn decision_box(
        &self,
        open: &OpenCard,
        width: usize,
        live: bool,
        typing_hint: bool,
    ) -> Vec<Line<'static>> {
        let t = &self.theme;
        let p = t.palette();
        let card = &open.card;
        let outer = width.clamp(24, BOX_WIDTH);
        let inner = outer - 4;
        let border = t.fg(p.brand);
        let strong = Style::new().bg(t.color(p.decision_strong));
        let body = Style::new().bg(t.color(p.decision));
        let mut rows: Vec<(Vec<Span<'static>>, Style)> = Vec::new();

        // Heading: the pill and what will happen; the risk and whether it can be taken back.
        let right = format!("{} · {}", card.risk_text, card.reversibility_text);
        let heading = format!("{} · {}", card.status_text, safe_line(&card.heading));
        let accent = t.accent().add_modifier(Modifier::BOLD);
        if heading.width() + right.width() + 2 <= inner {
            let gap = inner - heading.width() - right.width();
            rows.push((
                vec![
                    Span::styled(heading, accent),
                    Span::raw(" ".repeat(gap)),
                    Span::styled(right, t.accent()),
                ],
                strong,
            ));
        } else {
            // A narrow box: what will happen is wrapped whole, never cut (§7.1).
            for line in wrap_spans(vec![Span::styled(heading, accent)], inner, 0) {
                rows.push((line.spans, strong));
            }
            rows.push((vec![Span::styled(right, t.accent())], strong));
        }
        rows.push((Vec::new(), body));
        rows.push((
            vec![Span::styled(
                format!("{} · {}", safe_line(&card.agent.display_name), card.since_text),
                t.label(),
            )],
            body,
        ));

        // The exact call.
        rows.push((
            vec![Span::styled(safe_line(&card.call.tool), t.text().add_modifier(Modifier::BOLD))],
            body,
        ));
        let too_long = needs_details(open);
        if open.details {
            let pretty = serde_json::to_string_pretty(&card.call.args).unwrap_or_default();
            for line in safe_lines(&pretty) {
                for wrapped in wrap_spans(vec![Span::styled(line, t.text())], inner - 2, 0) {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(wrapped.spans);
                    rows.push((spans, body));
                }
            }
        } else {
            let arg_rows = self.compact_args(&card.call.args, inner - 2);
            let shown = if too_long { arg_rows.len().min(COMPACT_ROWS) } else { arg_rows.len() };
            for spans in arg_rows.iter().take(shown) {
                let mut row = vec![Span::raw("  ")];
                row.extend(spans.clone());
                rows.push((row, body));
            }
            if too_long {
                rows.push((
                    vec![Span::styled(
                        "  … press ? to see the whole call before you answer",
                        t.accent(),
                    )],
                    body,
                ));
            }
        }
        rows.push((Vec::new(), body));

        // How it got here.
        if open.details {
            for step in &card.timeline {
                let (mark, style) =
                    if step.current { ("● ", t.accent()) } else { ("○ ", t.label()) };
                rows.push((
                    vec![Span::styled(mark, style), Span::styled(step.text.clone(), t.secondary())],
                    body,
                ));
            }
            rows.push((Vec::new(), body));
            let mut details = format!("{} · {}", card.risk_text, card.reversibility_text);
            if let Some(also) = &card.also_shown {
                details.push_str(" · ");
                details.push_str(also);
            }
            for line in wrap_spans(vec![Span::styled(details, t.secondary())], inner, 0) {
                rows.push((line.spans, body));
            }
            rows.push((Vec::new(), body));
        } else {
            let steps: Vec<&str> = card.timeline.iter().map(|step| step.text.as_str()).collect();
            for line in wrap_spans(vec![Span::styled(steps.join(" · "), t.label())], inner, 0) {
                rows.push((line.spans, body));
            }
            rows.push((Vec::new(), body));
        }
        if let Some(note) = &card.allowed.note {
            for line in wrap_spans(vec![Span::styled(note.clone(), t.accent())], inner, 0) {
                rows.push((line.spans, body));
            }
            rows.push((Vec::new(), body));
        }

        // The answers, by key.
        let cap = if live { t.keycap() } else { t.keycap().add_modifier(Modifier::DIM) };
        let words_style = if live { t.text() } else { t.label() };
        let mut keys: Vec<Span<'static>> = Vec::new();
        for option in &card.allowed.options {
            keys.push(Span::styled(format!(" {} ", key_of(option.answer)), cap));
            keys.push(Span::styled(format!(" {}   ", option.label), words_style));
        }
        keys.push(Span::styled(" ? ", cap));
        keys.push(Span::styled(format!(" {DETAILS}"), words_style));
        for line in wrap_keys(keys, inner) {
            rows.push((line, body));
        }
        if typing_hint {
            rows.push((
                vec![Span::styled("Typing a message: Esc to answer by key.", t.label())],
                body,
            ));
        }
        rows.push((Vec::new(), body));

        let mut lines = Vec::with_capacity(rows.len() + 2);
        lines.push(Line::from(Span::styled(format!("┌{}┐", "─".repeat(outer - 2)), border)));
        for (spans, style) in rows {
            let used: usize = spans.iter().map(|span| span.content.width()).sum();
            let mut line = vec![Span::styled("│", border), Span::styled(" ", style)];
            for span in spans {
                let patched = style.patch(span.style);
                line.push(Span::styled(span.content, patched));
            }
            line.push(Span::styled(" ".repeat(inner.saturating_sub(used) + 1), style));
            line.push(Span::styled("│", border));
            lines.push(Line::from(line));
        }
        lines.push(Line::from(Span::styled(format!("└{}┘", "─".repeat(outer - 2)), border)));
        lines
    }

    /// The arguments as `key  value` rows, values wrapped under themselves.
    fn compact_args(&self, args: &Value, width: usize) -> Vec<Vec<Span<'static>>> {
        let t = &self.theme;
        let Some(map) = args.as_object() else {
            let text = match args {
                Value::Null => String::new(),
                other => other.to_string(),
            };
            return wrap_spans(vec![Span::styled(safe_line(&text), t.text())], width, 0)
                .into_iter()
                .map(|line| line.spans)
                .collect();
        };
        let key_width = map.keys().map(|key| safe_line(key).width()).max().unwrap_or(0).min(18);
        let mut rows = Vec::new();
        for (key, value) in map {
            let text = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            let key = fit(&safe_line(key), key_width);
            let pad = key_width - key.width() + 2;
            let spans = vec![
                Span::styled(key, t.label()),
                Span::raw(" ".repeat(pad)),
                Span::styled(safe_line(&text), t.text()),
            ];
            for line in wrap_spans(spans, width, key_width + 2) {
                rows.push(line.spans);
            }
        }
        rows
    }

    // -----------------------------------------------------------------------
    // The other views
    // -----------------------------------------------------------------------

    pub(crate) fn today(&self, app: &App, width: usize) -> Vec<Line<'static>> {
        let t = &self.theme;
        let mut lines = vec![Line::default()];
        let Some(briefing) = &app.briefing else {
            lines.push(Line::from(Span::styled(words::WHILE_YOU_WERE_AWAY, t.heading())));
            return lines;
        };
        lines.push(Line::from(vec![
            Span::styled(briefing.title.clone(), t.text().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", briefing.since_text), t.label()),
        ]));
        let mut summary = Vec::new();
        if let Some(spend) = &briefing.spend {
            summary.push(Span::styled(spend.text.clone(), t.text()));
            summary.push(Span::styled("  ·  ", t.label()));
        }
        summary.push(Span::styled(words::DECISIONS, t.text()));
        summary.push(Span::raw(" "));
        summary.push(Span::styled(format!(" {} ", briefing.waiting), t.badge()));
        lines.push(Line::from(summary));
        lines.push(Line::default());
        for line in &briefing.lines {
            let pill = line.pill_text.clone().unwrap_or_default();
            let style = line.pill.map_or(t.label(), |pill| self.pill_style(pill));
            let spans = vec![
                Span::styled(format!("{}  ", line.at_text), t.label()),
                Span::styled(format!("{pill:<16}"), style),
                Span::styled(safe_line(&line.text), t.secondary()),
            ];
            lines.extend(wrap_spans(spans, width, 23));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("esc to go back", t.label())));
        lines
    }

    pub(crate) fn permissions(&self, app: &App, width: usize) -> Vec<Line<'static>> {
        let t = &self.theme;
        let mut lines = vec![
            Line::default(),
            Line::from(Span::styled(
                "What your agents can do",
                t.text().add_modifier(Modifier::BOLD),
            )),
            Line::default(),
        ];
        let status = app.status.as_ref();
        let current = status.and_then(|status| status.mode.as_ref()).map(|mode| mode.mode);
        let mut modes = vec![Span::styled("How much they ask first  ", t.label())];
        for (key, mode, word) in [
            ('1', Mode::JustLook, words::JUST_LOOK),
            ('2', Mode::Suggest, words::SUGGEST),
            ('3', Mode::AskFirst, words::ASK_FIRST),
            ('4', Mode::GoAhead, words::GO_AHEAD),
        ] {
            modes.push(Span::styled(format!(" {key} "), t.keycap()));
            let style = if current == Some(mode) { t.badge() } else { t.text() };
            modes.push(Span::styled(format!(" {word} "), style));
            modes.push(Span::raw("  "));
        }
        lines.extend(wrap_keys(modes, width).into_iter().map(Line::from));
        let row = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("{label:<24}"), t.label()),
                Span::styled(value, t.text()),
            ])
        };
        let spend = status.and_then(|status| status.spend.as_ref()).map(|spend| spend.text.clone());
        lines.push(row(words::DAILY_SPENDING_LIMIT, spend.unwrap_or_else(|| "—".into())));
        let quiet = status.and_then(|status| status.quiet.as_ref()).map(|quiet| quiet.text.clone());
        lines.push(row(words::QUIET_TIME, quiet.unwrap_or_else(|| "—".into())));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            // The engine's rule for money (spec 3.3 §7.2), on every surface.
            "A call that spends money is never allowed always.",
            t.secondary(),
        )));
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(" ctrl-k ", t.stop()),
            Span::styled(format!(" {}", words::STOP_ALL), t.text()),
        ]));
        if let Some(stopped) = status.and_then(|status| status.stopped_text.clone()) {
            lines.push(Line::from(vec![
                Span::styled(stopped, t.fg(t.palette().failed)),
                Span::raw("   "),
                Span::styled(" r ", t.keycap()),
                Span::styled(format!(" {}", words::RESUME_ALL), t.text()),
            ]));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("esc to go back", t.label())));
        lines
    }

    pub(crate) fn help(&self) -> Vec<Line<'static>> {
        let t = &self.theme;
        let rows = [
            ("y", "Allow once"),
            ("a", "Always allow (the scope the decision names)"),
            ("e", "Edit the call's arguments, then allow"),
            ("n", "Deny, with a reason if you like"),
            ("?", "Details: the whole call, how it got here"),
            ("esc", "Never decides: switch between answering by key and typing"),
            ("tab", "The next agent's thread"),
            ("ctrl-t", "Today: while you were away"),
            ("ctrl-r", "Permissions: what your agents can do"),
            ("ctrl-k", "Stop all"),
            ("pgup pgdn", "Scroll the thread"),
            ("ctrl-c", "Leave; a kernel's agents carry on"),
            ("/mode", "/mode just look | suggest | ask first | go ahead"),
            ("/stop /resume", "Stop all, Resume all"),
            ("/session", "/session new, or /session ID"),
            ("/quit", "Leave"),
        ];
        let mut lines = vec![Line::default()];
        for (key, what) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("{key:<16}"), t.text().add_modifier(Modifier::BOLD)),
                Span::styled(what, t.secondary()),
            ]));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("esc to go back", t.label())));
        lines
    }

    // -----------------------------------------------------------------------
    // The message box
    // -----------------------------------------------------------------------

    pub(crate) fn prompt(&self, app: &App) -> (Span<'static>, String) {
        let t = &self.theme;
        let label = match &app.input {
            Input::Message => "› ".to_string(),
            Input::EditArgs { .. } => format!("{} › ", words::EDIT),
            Input::DenyReason { .. } => format!("{} — reason (optional) › ", words::DENY),
        };
        let text = safe_line(&app.composer.text);
        (Span::styled(label, t.accent().add_modifier(Modifier::BOLD)), text)
    }

    pub(crate) fn placeholder(&self, app: &App, now: Instant) -> Option<&'static str> {
        if !app.composer.is_empty() || app.input != Input::Message {
            return None;
        }
        if app.keys_answer(now) {
            Some("Answer with a key above, or Esc to type a message")
        } else if app.box_hidden && !app.cards.is_empty() {
            Some("Make the window taller, or PageDown, to answer the decision")
        } else {
            Some("Type a message · / for commands")
        }
    }

    pub(crate) fn queued(&self, queued: &[String], width: usize) -> Vec<Line<'static>> {
        let t = &self.theme;
        queued
            .iter()
            .flat_map(|text| {
                wrap_spans(
                    vec![
                        Span::styled("Queued › ", t.label()),
                        Span::styled(safe_line(text), t.secondary()),
                        Span::styled(
                            format!("   ({})", words::SENT_AFTER_THIS_CARD_IS_ANSWERED),
                            t.label(),
                        ),
                    ],
                    width,
                    9,
                )
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // The whole screen
    // -----------------------------------------------------------------------

    /// Draws the whole screen; returns whether the box the keys answer, if any, is wholly on
    /// screen (§7.1).
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>, app: &App, now: Instant) -> bool {
        let t = self.theme;
        let area = frame.area();
        frame.buffer_mut().set_style(area, t.base());
        let [status_area, body, footer_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
                .areas(area);

        let strong = Style::new().bg(t.color(t.palette().decision_strong));
        frame.render_widget(
            Paragraph::new(self.status_line(app)).style(t.base().patch(strong)),
            status_area,
        );
        let raised = Style::new().bg(t.color(t.palette().raised));
        frame.render_widget(
            Paragraph::new(self.footer()).style(t.base().patch(raised)),
            footer_area,
        );

        let left_width = match area.width {
            100.. => 34,
            72..=99 => 28,
            _ => 0,
        };
        let [left, divider, right] = Layout::horizontal([
            Constraint::Length(left_width),
            Constraint::Length(u16::from(left_width > 0)),
            Constraint::Min(20),
        ])
        .areas(body);
        if left_width > 0 {
            frame.render_widget(
                Paragraph::new(self.left(app, left.width.into())).style(t.base()),
                left,
            );
            for y in divider.top()..divider.bottom() {
                frame.buffer_mut()[(divider.x, y)].set_symbol("│").set_style(t.border());
            }
        }

        // The right pane: two cells of margin.
        let right = Rect { x: right.x + 2, width: right.width.saturating_sub(4).max(1), ..right };
        let width = usize::from(right.width);
        let queued = self.queued(&app.queued, width);
        let queued_height = u16::try_from(queued.len().min(3)).unwrap_or(3);
        let [main, queued_area, input] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(queued_height),
            Constraint::Length(3),
        ])
        .areas(right);

        let (lines, focused) = match app.view {
            View::Thread => self.thread(app, width, now),
            View::Today => (self.today(app, width), None),
            View::Permissions => (self.permissions(app, width), None),
            View::Help => (self.help(), None),
        };
        let height = usize::from(main.height);
        let scroll = if app.view == View::Thread {
            app.scroll.min(lines.len().saturating_sub(height))
        } else {
            0
        };
        let mut box_shown = focused.is_none();
        let visible: Vec<Line<'static>> = if app.view == View::Thread {
            let mut end = lines.len().saturating_sub(scroll);
            let mut start = end.saturating_sub(height);
            // Unscrolled, the box the keys answer is kept in view from its top, before what
            // follows it.
            if let Some(focused) = &focused
                && scroll == 0
                && focused.start < start
            {
                start = focused.start;
                end = (start + height).min(lines.len());
            }
            box_shown =
                focused.as_ref().is_none_or(|focused| focused.start >= start && focused.end <= end);
            lines[start..end].to_vec()
        } else {
            lines.into_iter().take(height).collect()
        };
        frame.render_widget(Paragraph::new(visible).style(t.base()), main);
        let tail = queued.len().saturating_sub(usize::from(queued_height));
        frame.render_widget(Paragraph::new(queued[tail..].to_vec()).style(t.base()), queued_area);

        let block = Block::new().borders(Borders::ALL).border_style(t.border()).style(t.base());
        let inner = block.inner(input);
        frame.render_widget(block, input);
        let (prompt, text) = self.prompt(app);
        let prompt_width = prompt.content.width();
        let room = usize::from(inner.width).saturating_sub(prompt_width + 1);
        let before: String = text.chars().take(app.composer.cursor).collect();
        let skip = before.width().saturating_sub(room);
        let shown: String = skip_width(&text, skip);
        let mut spans = vec![prompt];
        match self.placeholder(app, now) {
            Some(placeholder) => spans.push(Span::styled(placeholder, t.label())),
            None => spans.push(Span::styled(shown, t.text())),
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), inner);
        let cursor_x = inner.x + u16::try_from(prompt_width + before.width() - skip).unwrap_or(0);
        frame.set_cursor_position(Position::new(
            cursor_x.min(inner.right().saturating_sub(1)),
            inner.y,
        ));
        box_shown
    }
}

/// `text` cut to `width` cells, with an ellipsis when cut.
pub(crate) fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = UnicodeWidthStr::width(c.encode_utf8(&mut [0; 4]) as &str);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// `text` without its first `width` cells.
fn skip_width(text: &str, width: usize) -> String {
    let mut skipped = 0;
    text.chars()
        .skip_while(|c| {
            let w = UnicodeWidthStr::width(c.encode_utf8(&mut [0; 4]) as &str);
            if skipped + w <= width && width > 0 {
                skipped += w;
                true
            } else {
                false
            }
        })
        .collect()
}

/// Key hints laid out in rows, never splitting a key from its words.
fn wrap_keys(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0;
    let mut pairs = spans.chunks(2);
    for pair in pairs.by_ref() {
        let w: usize = pair.iter().map(|span| span.content.width()).sum();
        if used + w > width && used > 0 {
            rows.push(Vec::new());
            used = 0;
        }
        used += w;
        if let Some(row) = rows.last_mut() {
            row.extend(pair.iter().cloned());
        }
    }
    rows
}
