//! The terminal's Agents view (spec 3.3 §5.2, §5.7): the person's own agents, added, changed,
//! paused and removed through the `/agent` command the terminal has — in process, or a
//! kernel's through its command routes — so the view and the REPL run the same code.
//!
//! | Key | In the Agents view |
//! |---|---|
//! | ↑ ↓ | Select an agent |
//! | Enter | Open its latest thread |
//! | `n` | New agent: **Describe it** or **Set it up yourself** |
//! | `e` | Settings: the definition's form, filled in |
//! | `p` | Pause, or Resume a paused agent |
//! | `x` | Remove, after naming what ends |
//! | Esc | Back; in the form, leave without saving |
//!
//! In the form, Tab moves to the next field, Enter shows **Here is what Finance can do**, and a
//! second Enter adds it. None of these keys answers a decision: decision keys answer only in
//! the thread (§7.4). Names and instructions pass `terminal_safe` (S-I9).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::safe::safe_line;
use super::theme::Theme;
use crate::agents::words as agent_words;
use crate::model::AgentRow;

/// The command the view runs.
pub(crate) const AGENT_COMMAND: &str = "agent";

/// What **Describe it** holds in the message box of a new thread with the primary agent.
pub(crate) const DESCRIBE_PREFIX: &str = agent_words::SUGGEST_A_NEW_AGENT_FOR;

/// One of the person's agents, as `/agent list --json` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    pub id: String,
    pub name: String,
    pub state: String,
    pub area: String,
    pub mode: String,
    pub cant_start: Option<String>,
    /// The agent's definition, for its settings.
    pub definition: Value,
    pub version: u64,
}

/// A field of the form, in the order of spec 3.3 §5.7 "The definition".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    /// The `/agent` flag it sets.
    pub flag: &'static str,
    pub label: &'static str,
    pub value: String,
    /// The values a choice cycles through; empty for text.
    pub choices: &'static [&'static str],
}

const MODES: &[&str] = &["just-look", "suggest", "ask-first", "go-ahead"];
const FILES: &[&str] = &["none", "read", "read-write"];
const SHELLS: &[&str] = &["none", "sandbox", "host"];
const COLOURS: &[&str] = &["orange", "green", "blue", "violet", "slate", "amber", "teal", "pink"];

fn fields() -> Vec<Field> {
    let text = |flag, label| Field { flag, label, value: String::new(), choices: &[] };
    let choice = |flag, label, choices: &'static [&'static str], value: &str| Field {
        flag,
        label,
        value: value.to_string(),
        choices,
    };
    vec![
        text("name", "Name"),
        choice("colour", "Colour", COLOURS, "green"),
        text("does", "What it does"),
        text("instructions", "Instructions"),
        text("area", "Area"),
        choice("asks", "How much it asks first", MODES, "ask-first"),
        text("model", "Runs on (provider/model)"),
        choice("files", "Files", FILES, "none"),
        text("folder", "Folder"),
        choice("shell", "Shell", SHELLS, "none"),
        text("web", "The web (none, any, or sites)"),
        text("pack", "Packs"),
        text("driver", "Drivers"),
        text("limit", "Daily spending limit ($)"),
    ]
}

/// The form of **Set it up yourself** and an agent's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Form {
    /// The agent the form changes; `None` for a new one.
    pub editing: Option<String>,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// The preview of what the form holds now, once Enter asked for it.
    pub preview: Option<Preview>,
}

/// **Here is what Finance can do**, or what removing an agent ends, with the code that
/// confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preview {
    pub heading: String,
    pub lines: Vec<String>,
    pub risk: String,
    pub code: String,
    pub needs_presence: bool,
    /// The command line it confirms, without `--confirm`.
    pub args: String,
}

/// What the view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pane {
    List,
    /// **New agent**: Describe it (`true`) or Set it up yourself.
    Choose {
        describe: bool,
    },
    Form(Form),
    /// Pause, resume or remove, previewed and waiting for Enter.
    Confirm(Preview),
}

/// What the request in flight is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Purpose {
    List,
    Preview { args: String },
    Apply,
}

/// What the view asks the terminal to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Run `/agent args` for `purpose`.
    Run { args: String, purpose: Purpose },
    /// Open a new thread with the primary agent holding `text` in the message box.
    Describe { text: String },
    /// Open the thread `session`.
    Open { session: String },
    /// Open a new thread with `agent`, which has none yet.
    Start { agent: String },
    /// Back to the thread.
    Close,
}

/// The Agents view's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentsPane {
    pub listed: Vec<Listed>,
    pub selected: usize,
    pub pane: Pane,
    /// A line under the list: what the last change did, or why it was refused.
    pub note: Option<(String, bool)>,
    /// The request in flight: its serial and what it is for.
    pub waiting: Option<(u64, Purpose)>,
    /// The terminal has no kernel: what `/agent` answered.
    pub no_kernel: Option<String>,
}

impl Default for AgentsPane {
    fn default() -> Self {
        Self {
            listed: Vec::new(),
            selected: 0,
            pane: Pane::List,
            note: None,
            waiting: None,
            no_kernel: None,
        }
    }
}

/// `value` as one `/agent` word: quoted when it holds a space or a quote.
pub(crate) fn quote(value: &str) -> String {
    if !value.is_empty() && !value.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Form {
    fn new(editing: Option<String>) -> Self {
        Self { editing, fields: fields(), focus: 0, preview: None }
    }

    /// The form of `listed`'s settings, filled in from its definition.
    fn of(listed: &Listed) -> Self {
        let mut form = Self::new(Some(listed.id.clone()));
        let definition = &listed.definition;
        let text =
            |key: &str| definition.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let uses = definition.get("uses").cloned().unwrap_or(Value::Null);
        for field in &mut form.fields {
            field.value = match field.flag {
                "name" => text("displayName"),
                "colour" => text("color"),
                "does" => text("description"),
                "instructions" => text("instructions"),
                "area" => text("area"),
                "asks" => match text("mode").as_str() {
                    "readOnly" => "just-look",
                    "plan" => "suggest",
                    "auto" => "go-ahead",
                    _ => "ask-first",
                }
                .to_string(),
                "model" => definition
                    .get("model")
                    .and_then(|model| {
                        Some(format!(
                            "{}/{}",
                            model.get("provider")?.as_str()?,
                            model.get("model")?.as_str()?
                        ))
                    })
                    .unwrap_or_default(),
                "files" => match uses.get("files").and_then(Value::as_str) {
                    Some("read") => "read",
                    Some("readWrite") => "read-write",
                    _ => "none",
                }
                .to_string(),
                "folder" => {
                    uses.get("folder").and_then(Value::as_str).unwrap_or_default().to_string()
                }
                "shell" => match uses.get("shell").and_then(Value::as_str) {
                    Some("sandboxed") => "sandbox",
                    Some("host") => "host",
                    _ => "none",
                }
                .to_string(),
                "web" => {
                    match uses.get("web").and_then(|web| web.get("access")).and_then(Value::as_str)
                    {
                        Some("any") => "any".to_string(),
                        Some("listed") => uses["web"]["hosts"]
                            .as_array()
                            .map(|hosts| {
                                hosts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")
                            })
                            .unwrap_or_default(),
                        _ => "none".to_string(),
                    }
                }
                "pack" | "driver" => uses
                    .get(if field.flag == "pack" { "packs" } else { "drivers" })
                    .and_then(Value::as_array)
                    .map(|items| {
                        items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")
                    })
                    .unwrap_or_default(),
                "limit" => definition
                    .get("dailyLimitMicroUsd")
                    .and_then(Value::as_u64)
                    .map(|limit| format!("{:.2}", limit as f64 / 1_000_000.0))
                    .unwrap_or_default(),
                _ => String::new(),
            };
        }
        form
    }

    /// The name the form holds.
    fn name(&self) -> String {
        self.fields.first().map(|field| field.value.trim().to_string()).unwrap_or_default()
    }

    /// The `/agent` line the form holds.
    fn args(&self) -> String {
        let mut words = match &self.editing {
            Some(id) => vec!["edit".to_string(), quote(id)],
            None => vec!["new".to_string()],
        };
        for field in &self.fields {
            let value = field.value.trim();
            match field.flag {
                // Lists go one flag per item, and an empty one clears them when editing.
                "pack" | "driver" => {
                    let items: Vec<&str> =
                        value.split(',').map(str::trim).filter(|item| !item.is_empty()).collect();
                    if items.is_empty() && self.editing.is_some() {
                        words.push(format!("--{}", field.flag));
                        words.push("none".into());
                    }
                    for item in items {
                        words.push(format!("--{}", field.flag));
                        words.push(quote(item));
                    }
                }
                "limit" if value.is_empty() && self.editing.is_some() => {
                    words.push("--limit".into());
                    words.push("none".into());
                }
                "folder" if value.is_empty() && self.editing.is_some() => {
                    words.push("--folder".into());
                    words.push("\"\"".into());
                }
                _ if value.is_empty() => {}
                _ => {
                    words.push(format!("--{}", field.flag));
                    words.push(quote(value));
                }
            }
        }
        words.join(" ")
    }
}

/// The preview `/agent … --json` answered, for the line `args`.
fn preview_of(answer: &Value, args: &str) -> Option<Preview> {
    let preview = answer.get("preview")?;
    let name = answer.get("name").and_then(Value::as_str).unwrap_or_default();
    let code = answer.get("code").and_then(Value::as_str)?.to_string();
    let risk = match preview.get("risk").and_then(Value::as_str) {
        Some("low") => crate::words::LOW_RISK,
        Some("medium") => crate::words::MEDIUM_RISK,
        _ => crate::words::HIGH_RISK,
    };
    let money = preview.get("money").and_then(Value::as_bool).unwrap_or(false);
    let widens = preview.get("widens").and_then(Value::as_bool).unwrap_or(false);
    let high = matches!(preview.get("risk").and_then(Value::as_str), Some("high" | "critical"));
    let (heading, lines) = match preview.get("ended").filter(|ended| !ended.is_null()) {
        Some(ended) => {
            let count = |key: &str| {
                ended
                    .get(key)
                    .and_then(Value::as_array)
                    .map_or(0, |items| u32::try_from(items.len()).unwrap_or(u32::MAX))
            };
            let work = crate::agents::EndedWork {
                work: count("work"),
                routines: count("routines"),
                decisions: count("decisions"),
                follow_ups: count("followUps"),
                rules: u32::try_from(ended.get("rules").and_then(Value::as_u64).unwrap_or(0))
                    .unwrap_or(u32::MAX),
                text: String::new(),
            };
            (
                format!("Remove {}?", safe_line(name)),
                vec![agent_words::removing_ends(&safe_line(name), &work)],
            )
        }
        None => (
            agent_words::here_is_what_can_do(&safe_line(name)),
            preview
                .get("abilities")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).map(safe_line).collect())
                .unwrap_or_default(),
        ),
    };
    Some(Preview {
        heading,
        lines,
        risk: if money { format!("{risk} · spends money") } else { risk.to_string() },
        code,
        needs_presence: widens && (high || money),
        args: args.to_string(),
    })
}

fn listed_of(view: &Value) -> Option<Listed> {
    let agent = view.get("agent")?;
    let text = |key: &str| agent.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let cant_start = view.get("cantStart").and_then(Value::as_str).map(str::to_string);
    let state = match (cant_start.is_some(), text("state").as_str()) {
        (true, _) => agent_words::CANT_START,
        (_, "proposed") => "Suggested",
        (_, "paused") => agent_words::PAUSED,
        (_, "removing") => "Removing",
        _ => agent_words::READY,
    };
    let mode = match text("mode").as_str() {
        "readOnly" => "Just looks",
        "plan" => "Suggests",
        "auto" => "Goes ahead",
        _ => "Asks first",
    };
    Some(Listed {
        id: text("id"),
        name: safe_line(&text("displayName")),
        state: state.to_string(),
        area: safe_line(&text("area")),
        mode: mode.to_string(),
        cant_start: cant_start.map(|reason| safe_line(&reason)),
        definition: agent.clone(),
        version: agent.get("version").and_then(Value::as_u64).unwrap_or(0),
    })
}

impl AgentsPane {
    /// Opening the view: read the agents again.
    pub(crate) fn open(&mut self) -> Vec<Ask> {
        self.pane = Pane::List;
        self.note = None;
        vec![Ask::Run { args: "list --json".into(), purpose: Purpose::List }]
    }

    /// The request `serial` was sent for `purpose`.
    pub(crate) fn sent(&mut self, serial: u64, purpose: Purpose) {
        self.waiting = Some((serial, purpose));
    }

    /// Whether the answer to `serial` is this view's.
    pub(crate) fn takes(&self, serial: u64) -> bool {
        self.waiting.as_ref().is_some_and(|(waiting, _)| *waiting == serial)
    }

    fn selected(&self) -> Option<&Listed> {
        self.listed.get(self.selected)
    }

    /// The answer to the request in flight.
    pub(crate) fn done(&mut self, result: Result<String, String>) {
        let Some((_, purpose)) = self.waiting.take() else {
            return;
        };
        let text = match result {
            Ok(text) => text,
            Err(why) => {
                self.note = Some((safe_line(&why), true));
                return;
            }
        };
        let Ok(answer) = serde_json::from_str::<Value>(&text) else {
            // Without a kernel `/agent` answers in words: start `adk-rust up`.
            self.no_kernel = Some(safe_line(&text));
            self.pane = Pane::List;
            return;
        };
        self.no_kernel = None;
        match purpose {
            Purpose::List => {
                self.listed = answer
                    .get("agents")
                    .and_then(Value::as_array)
                    .map(|views| views.iter().filter_map(listed_of).collect())
                    .unwrap_or_default();
                self.selected = self.selected.min(self.listed.len().saturating_sub(1));
            }
            Purpose::Preview { args } => {
                let Some(preview) = preview_of(&answer, &args) else {
                    self.note = Some(("The kernel answered without a preview.".into(), true));
                    return;
                };
                match &mut self.pane {
                    Pane::Form(form) => form.preview = Some(preview),
                    _ => self.pane = Pane::Confirm(preview),
                }
            }
            Purpose::Apply => {
                let outcome = answer.get("outcome").cloned().unwrap_or(Value::Null);
                let kind = outcome.get("outcome").and_then(Value::as_str).unwrap_or_default();
                let agent = outcome.get("agent").cloned().unwrap_or(Value::Null);
                let name =
                    safe_line(agent.get("displayName").and_then(Value::as_str).unwrap_or_default());
                let id = agent.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                let note = match kind {
                    "created" => format!("You added {name}."),
                    "changed" => format!("Saved {name}."),
                    "paused" => agent_words::is_paused(&name),
                    "resumed" => format!("{name} runs again."),
                    "removed" => format!("You removed {name}. Its threads stay."),
                    _ => "Done.".to_string(),
                };
                let view = serde_json::json!({ "agent": agent });
                match (kind, listed_of(&view)) {
                    ("removed", _) => self.listed.retain(|listed| listed.id != id),
                    (_, Some(listed)) => {
                        match self.listed.iter_mut().find(|known| known.id == id) {
                            Some(known) => *known = listed,
                            None => {
                                self.listed.push(listed);
                                self.selected = self.listed.len() - 1;
                            }
                        }
                    }
                    _ => {}
                }
                self.selected = self.selected.min(self.listed.len().saturating_sub(1));
                self.note = Some((note, false));
                self.pane = Pane::List;
            }
        }
    }

    /// A key while the view shows; `None` when the view leaves it to the terminal.
    pub(crate) fn key(&mut self, key: KeyEvent, roster: &[AgentRow]) -> Option<Vec<Ask>> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        // One request at a time: keys wait for its answer, Esc excepted.
        if self.waiting.is_some() && key.code != KeyCode::Esc {
            return Some(Vec::new());
        }
        match &mut self.pane {
            Pane::List => self.list_key(key, roster),
            Pane::Choose { describe } => match key.code {
                KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                    *describe = !*describe;
                    Some(Vec::new())
                }
                KeyCode::Char('d') => Some(vec![Ask::Describe { text: DESCRIBE_PREFIX.into() }]),
                KeyCode::Char('s') => {
                    self.pane = Pane::Form(Form::new(None));
                    Some(Vec::new())
                }
                KeyCode::Enter if *describe => {
                    Some(vec![Ask::Describe { text: DESCRIBE_PREFIX.into() }])
                }
                KeyCode::Enter => {
                    self.pane = Pane::Form(Form::new(None));
                    Some(Vec::new())
                }
                KeyCode::Esc => {
                    self.pane = Pane::List;
                    Some(Vec::new())
                }
                _ => Some(Vec::new()),
            },
            Pane::Form(form) => Some(form_key(form, key, &mut self.note).unwrap_or_else(|| {
                // Esc never saves.
                self.pane = Pane::List;
                Vec::new()
            })),
            Pane::Confirm(preview) => match key.code {
                KeyCode::Enter | KeyCode::Char('x') | KeyCode::Char('p') => {
                    let args = format!("{} --confirm {} --json", preview.args, preview.code);
                    Some(vec![Ask::Run { args, purpose: Purpose::Apply }])
                }
                KeyCode::Esc | KeyCode::Char('k') => {
                    self.pane = Pane::List;
                    Some(Vec::new())
                }
                _ => Some(Vec::new()),
            },
        }
    }

    fn list_key(&mut self, key: KeyEvent, roster: &[AgentRow]) -> Option<Vec<Ask>> {
        let count = self.listed.len();
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                Some(Vec::new())
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
                Some(Vec::new())
            }
            KeyCode::Enter => {
                let id = self.selected()?.id.clone();
                let session = roster
                    .iter()
                    .find(|row| row.agent.name == id)
                    .and_then(|row| row.session_id.clone());
                match session {
                    Some(session) => Some(vec![Ask::Open { session }]),
                    None => Some(vec![Ask::Start { agent: id }]),
                }
            }
            KeyCode::Char('n') => {
                self.pane = Pane::Choose { describe: true };
                self.note = None;
                Some(Vec::new())
            }
            KeyCode::Char('e') => {
                let form = Form::of(self.selected()?);
                self.pane = Pane::Form(form);
                self.note = None;
                Some(Vec::new())
            }
            KeyCode::Char('p') => {
                let listed = self.selected()?;
                let verb = if listed.state == agent_words::PAUSED { "resume" } else { "pause" };
                let args = format!("{verb} {}", quote(&listed.id));
                Some(vec![Ask::Run {
                    args: format!("{args} --json"),
                    purpose: Purpose::Preview { args },
                }])
            }
            KeyCode::Char('x') => {
                let args = format!("remove {}", quote(&self.selected()?.id));
                Some(vec![Ask::Run {
                    args: format!("{args} --json"),
                    purpose: Purpose::Preview { args },
                }])
            }
            KeyCode::Esc => Some(vec![Ask::Close]),
            // Any other key does nothing here: no key of this view answers a decision.
            _ => Some(Vec::new()),
        }
    }
}

/// A key in the form; `None` when Esc leaves it.
fn form_key(form: &mut Form, key: KeyEvent, note: &mut Option<(String, bool)>) -> Option<Vec<Ask>> {
    let count = form.fields.len();
    let edit = |form: &mut Form| form.preview = None;
    match key.code {
        KeyCode::Esc => None,
        KeyCode::Tab | KeyCode::Down => {
            form.focus = (form.focus + 1) % count;
            Some(Vec::new())
        }
        KeyCode::BackTab | KeyCode::Up => {
            form.focus = (form.focus + count - 1) % count;
            Some(Vec::new())
        }
        KeyCode::Enter => {
            if form.name().is_empty() {
                *note = Some(("Give it a name first.".to_string(), true));
                return Some(Vec::new());
            }
            match &form.preview {
                // The second Enter confirms what the first one showed.
                Some(preview) => Some(vec![Ask::Run {
                    args: format!("{} --confirm {} --json", preview.args, preview.code),
                    purpose: Purpose::Apply,
                }]),
                None => {
                    let args = form.args();
                    Some(vec![Ask::Run {
                        args: format!("{args} --json"),
                        purpose: Purpose::Preview { args },
                    }])
                }
            }
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
            if !form.fields[form.focus].choices.is_empty() =>
        {
            let field = &mut form.fields[form.focus];
            let at = field.choices.iter().position(|choice| *choice == field.value).unwrap_or(0);
            let len = field.choices.len();
            let next =
                if key.code == KeyCode::Left { (at + len - 1) % len } else { (at + 1) % len };
            field.value = field.choices[next].to_string();
            edit(form);
            Some(Vec::new())
        }
        KeyCode::Backspace => {
            if form.fields[form.focus].choices.is_empty() {
                form.fields[form.focus].value.pop();
                edit(form);
            }
            Some(Vec::new())
        }
        KeyCode::Char(c) => {
            if form.fields[form.focus].choices.is_empty() {
                form.fields[form.focus].value.push(c);
                edit(form);
            }
            Some(Vec::new())
        }
        _ => Some(Vec::new()),
    }
}

/// The view, drawn.
pub(crate) fn lines(pane: &AgentsPane, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let t = theme;
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(
            crate::words::YOUR_AGENTS.to_string(),
            t.text().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
    ];
    let keys = |pairs: &[(&str, &str)]| {
        let mut spans = Vec::new();
        for (key, what) in pairs {
            spans.push(Span::styled(format!(" {key} "), t.keycap()));
            spans.push(Span::styled(format!(" {what}  "), t.label()));
        }
        Line::from(spans)
    };
    if let Some(text) = &pane.no_kernel {
        lines.push(Line::from(Span::styled(text.clone(), t.text())));
        lines.push(Line::default());
        lines.push(keys(&[("esc", "back")]));
        return lines;
    }
    match &pane.pane {
        Pane::List => {
            if pane.listed.is_empty() {
                lines.push(Line::from(Span::styled(
                    agent_words::NO_AGENTS_OF_YOUR_OWN.to_string(),
                    t.label(),
                )));
            }
            for (index, listed) in pane.listed.iter().enumerate() {
                let selected = index == pane.selected;
                let marker = if selected { "› " } else { "  " };
                let style = if selected { t.text().add_modifier(Modifier::BOLD) } else { t.text() };
                lines.push(Line::from(vec![
                    Span::styled(marker.to_string(), t.accent()),
                    Span::styled(format!("{:<24}", listed.name), style),
                    Span::styled(format!("{:<12}", listed.state), t.secondary()),
                    Span::styled(format!("{:<10}", listed.area), t.label()),
                    Span::styled(listed.mode.clone(), t.label()),
                ]));
                if let Some(reason) = &listed.cant_start {
                    lines.push(Line::from(Span::styled(format!("    {reason}"), t.label())));
                }
            }
            lines.push(Line::default());
            lines.push(keys(&[
                ("↑↓", "select"),
                ("enter", "open thread"),
                ("n", agent_words::NEW_AGENT),
                ("e", "settings"),
                ("p", "pause or resume"),
                ("x", agent_words::REMOVE),
                ("esc", "back"),
            ]));
        }
        Pane::Choose { describe } => {
            lines.push(Line::from(Span::styled(
                agent_words::NEW_AGENT.to_string(),
                t.text().add_modifier(Modifier::BOLD),
            )));
            for (on, key, label, hint) in [
                (
                    *describe,
                    "d",
                    agent_words::DESCRIBE_IT,
                    "opens a thread with ADK-Rust to suggest one",
                ),
                (!*describe, "s", agent_words::SET_IT_UP_YOURSELF, "fill in the form"),
            ] {
                lines.push(Line::from(vec![
                    Span::styled(if on { "› " } else { "  " }.to_string(), t.accent()),
                    Span::styled(format!(" {key} "), t.keycap()),
                    Span::styled(
                        format!(" {label}"),
                        if on { t.text().add_modifier(Modifier::BOLD) } else { t.text() },
                    ),
                    Span::styled(format!("  {hint}"), t.label()),
                ]));
            }
            lines.push(Line::default());
            lines.push(keys(&[("enter", "choose"), ("esc", "back")]));
        }
        Pane::Form(form) => {
            let heading = match &form.editing {
                Some(_) => format!("{}'s settings", safe_line(&form.name())),
                None => agent_words::SET_IT_UP_YOURSELF.to_string(),
            };
            lines.push(Line::from(Span::styled(heading, t.text().add_modifier(Modifier::BOLD))));
            for (index, field) in form.fields.iter().enumerate() {
                let focused = index == form.focus;
                let value = if field.choices.is_empty() {
                    safe_line(&field.value)
                } else {
                    format!("‹ {} ›", field.value)
                };
                let cursor = if focused && field.choices.is_empty() { "▏" } else { "" };
                lines.push(Line::from(vec![
                    Span::styled(if focused { "› " } else { "  " }.to_string(), t.accent()),
                    Span::styled(format!("{:<28}", field.label), t.label()),
                    Span::styled(
                        format!("{value}{cursor}"),
                        if focused { t.text().add_modifier(Modifier::BOLD) } else { t.text() },
                    ),
                ]));
            }
            lines.push(Line::default());
            match &form.preview {
                Some(preview) => {
                    push_preview(&mut lines, preview, t, width);
                    let confirm = match &form.editing {
                        Some(_) => agent_words::SAVE_CHANGES.to_string(),
                        None => agent_words::add_named(&safe_line(&form.name())),
                    };
                    lines.push(keys(&[("enter", &confirm), ("esc", "leave without saving")]));
                }
                None => lines.push(keys(&[
                    ("tab", "next field"),
                    ("←→", "choose"),
                    ("enter", "show what it can do"),
                    ("esc", "leave without saving"),
                ])),
            }
        }
        Pane::Confirm(preview) => {
            push_preview(&mut lines, preview, t, width);
            let action =
                if preview.heading.starts_with("Remove") { agent_words::REMOVE } else { "Confirm" };
            lines.push(keys(&[("enter", action), ("esc", agent_words::KEEP)]));
        }
    }
    if let Some((note, problem)) = &pane.note {
        lines.push(Line::default());
        let style = if *problem { t.fg(t.palette().failed) } else { t.secondary() };
        lines.push(Line::from(Span::styled(note.clone(), style)));
    }
    if pane.waiting.is_some() {
        lines.push(Line::from(Span::styled("Asking the kernel…".to_string(), t.label())));
    }
    lines
}

fn push_preview(lines: &mut Vec<Line<'static>>, preview: &Preview, t: &Theme, width: usize) {
    lines.push(Line::from(Span::styled(
        preview.heading.clone(),
        t.text().add_modifier(Modifier::BOLD),
    )));
    for line in &preview.lines {
        for part in wrap_words(line, width.saturating_sub(4)) {
            lines.push(Line::from(vec![Span::raw("  "), Span::styled(part, t.text())]));
        }
    }
    lines.push(Line::from(vec![
        Span::styled(preview.risk.clone(), t.secondary()),
        Span::styled(format!(" · {}", agent_words::YOU_CAN_REMOVE_IT_LATER), t.label()),
    ]));
    if preview.needs_presence {
        lines.push(Line::from(Span::styled(
            "High risk: this needs the kernel's own terminal, or the web home after confirming it's you.".to_string(),
            t.accent(),
        )));
    }
    lines.push(Line::default());
}

/// `text` in lines of at most `width` cells, broken between words.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr as _;
    let width = width.max(10);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.width() + 1 + word.width() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEventKind;
    use serde_json::json;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn listed() -> Value {
        json!({ "agents": [{ "agent": {
            "id": "finance", "displayName": "Finance\u{202e}", "color": "green", "description": "Reads invoices",
            "instructions": "Draft reminders.", "area": "default", "mode": "ask",
            "uses": { "files": "read", "folder": "Invoices", "shell": "none", "web": { "access": "none" } },
            "state": "active", "version": 2
        } }] })
    }

    #[test]
    fn the_list_escapes_names_and_says_ready() {
        let mut pane = AgentsPane::default();
        let asks = pane.open();
        assert_eq!(asks, [Ask::Run { args: "list --json".into(), purpose: Purpose::List }]);
        pane.sent(1, Purpose::List);
        assert!(pane.takes(1));
        pane.done(Ok(listed().to_string()));
        assert_eq!(pane.listed[0].name, "Finance\\u{202e}");
        assert_eq!(pane.listed[0].state, "Ready");
    }

    #[test]
    fn without_a_kernel_the_view_says_so() {
        let mut pane = AgentsPane::default();
        pane.sent(1, Purpose::List);
        pane.done(Ok(
            "Your own agents run on the always-on kernel. Start it with adk-rust up.".into()
        ));
        assert!(pane.no_kernel.as_deref().unwrap().contains("adk-rust up"));
    }

    #[test]
    fn the_form_previews_then_confirms_and_esc_never_saves() {
        let mut pane = AgentsPane::default();
        pane.key(key(KeyCode::Char('n')), &[]).unwrap();
        pane.key(key(KeyCode::Char('s')), &[]).unwrap();
        for c in "Finance".chars() {
            pane.key(key(KeyCode::Char(c)), &[]).unwrap();
        }
        // Tab to Files (the eighth field) and choose read.
        for _ in 0..7 {
            pane.key(key(KeyCode::Tab), &[]).unwrap();
        }
        pane.key(key(KeyCode::Right), &[]).unwrap();
        let asks = pane.key(key(KeyCode::Enter), &[]).unwrap();
        let Ask::Run { args, purpose } = &asks[0] else { panic!("{asks:?}") };
        assert_eq!(
            args,
            "new --name Finance --colour green --asks ask-first --files read --shell none --json"
        );
        pane.sent(2, purpose.clone());
        pane.done(Ok(json!({
            "preview": { "abilities": ["It can read files in the workspace."], "risk": "medium", "money": false, "widens": true },
            "code": "3f9a2c", "name": "Finance"
        })
        .to_string()));
        let Pane::Form(form) = &pane.pane else { panic!() };
        assert_eq!(form.preview.as_ref().unwrap().heading, "Here is what Finance can do");
        let asks = pane.key(key(KeyCode::Enter), &[]).unwrap();
        assert!(
            matches!(&asks[0], Ask::Run { args, purpose: Purpose::Apply } if args.ends_with("--confirm 3f9a2c --json"))
        );
        // A change after the preview needs a new one.
        pane.waiting = None;
        pane.key(key(KeyCode::Right), &[]).unwrap();
        let Pane::Form(form) = &pane.pane else { panic!() };
        assert!(form.preview.is_none());
        assert_eq!(pane.key(key(KeyCode::Esc), &[]).unwrap(), Vec::new());
        assert_eq!(pane.pane, Pane::List);
    }

    #[test]
    fn remove_names_what_ends_before_it_acts() {
        let mut pane = AgentsPane::default();
        pane.sent(1, Purpose::List);
        pane.done(Ok(listed().to_string()));
        let asks = pane.key(key(KeyCode::Char('x')), &[]).unwrap();
        assert_eq!(
            asks,
            [Ask::Run {
                args: "remove finance --json".into(),
                purpose: Purpose::Preview { args: "remove finance".into() }
            }]
        );
        pane.sent(2, Purpose::Preview { args: "remove finance".into() });
        pane.done(Ok(json!({
            "preview": { "abilities": [], "risk": "low", "ended": { "work": [], "decisions": ["d1"], "routines": ["r1"], "followUps": [], "rules": 0 } },
            "code": "abcdef", "name": "Bills"
        })
        .to_string()));
        let Pane::Confirm(preview) = &pane.pane else { panic!("{:?}", pane.pane) };
        assert_eq!(
            preview.lines,
            ["Removing Bills ends 1 decision waiting for you and 1 routine. Its threads stay."]
        );
        assert_eq!(pane.key(key(KeyCode::Esc), &[]).unwrap(), Vec::new());
        assert_eq!(pane.pane, Pane::List, "Esc keeps it");
    }

    #[test]
    fn settings_fill_in_the_definition() {
        let mut pane = AgentsPane::default();
        pane.sent(1, Purpose::List);
        pane.done(Ok(listed().to_string()));
        pane.key(key(KeyCode::Char('e')), &[]).unwrap();
        let Pane::Form(form) = &pane.pane else { panic!() };
        let value =
            |flag: &str| form.fields.iter().find(|field| field.flag == flag).unwrap().value.clone();
        assert_eq!(value("files"), "read");
        assert_eq!(value("folder"), "Invoices");
        assert_eq!(value("asks"), "ask-first");
        assert!(form.args().starts_with("edit finance --name"));
    }

    #[test]
    fn describe_it_opens_a_thread_holding_the_request() {
        let mut pane = AgentsPane::default();
        pane.key(key(KeyCode::Char('n')), &[]).unwrap();
        assert_eq!(
            pane.key(key(KeyCode::Enter), &[]).unwrap(),
            [Ask::Describe { text: "Suggest a new agent for this: ".into() }]
        );
    }

    #[test]
    fn values_with_spaces_are_quoted() {
        assert_eq!(quote("Finance team"), "\"Finance team\"");
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
    }
}
