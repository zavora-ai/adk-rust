//! `/agent`: the kernel's agent command (spec 3.3 §5.7, §10 L1).
//!
//! One command in the REPL's command set, so the REPL, the full-screen terminal and a kernel's
//! control routes (`POST /kernel/commands/run`) run the same code. Every change goes through
//! the kernel's audited agent table (`adk_kernel::agents::Agents`) and is shown first: without
//! `--confirm` a change prints **Here is what Finance can do** and the line that confirms it;
//! with `--confirm <code>` matching the preview's code it applies with the preview's digest.
//!
//! | Verb | What it does |
//! |---|---|
//! | `list` | The agents a person added, with their state, area and mode |
//! | `show AGENT` | One agent's definition |
//! | `new --name NAME [fields]` | Adds an agent by hand |
//! | `edit AGENT [fields]` | Changes an agent at the version shown |
//! | `pause AGENT`, `resume AGENT` | Pauses or resumes one agent |
//! | `remove AGENT` | Removes an agent, naming what ends |
//!
//! Fields: `--name`, `--does`, `--instructions`, `--area`, `--asks`, `--model`, `--files`,
//! `--folder`, `--shell`, `--web`, `--pack`, `--driver`, `--limit`, `--colour`. `--json`
//! answers in JSON, for the terminal's Agents view and `adk-rust agents --output json`.

use std::fmt::Write as _;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_kernel::PartitionMode;
use adk_kernel::agents::{
    AgentAction, AgentCaller, AgentDefinition, AgentPreview, AgentRecord, AgentRecordState,
    AgentUses, AgentView, Agents, AgentsHandle, AvatarColor, FileAccess, ModelChoice, Presence,
    ShellAccess, WebAccess,
};
use adk_kernel::control::{CommandReach, ControlTransport};
use serde_json::json;

/// What `/agent` answers without a kernel (spec 3.3 §10 L1).
pub(crate) const NO_KERNEL: &str =
    "Your own agents run on the always-on kernel. Start it with adk-rust up.";

/// The command's one-line description in `/help`.
pub(crate) const DESCRIPTION: &str =
    "Your own agents: /agent list|show|new|edit|pause|resume|remove";

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        "harness.agent.invalid",
        message,
    )
}

/// The words of `args`: whitespace separates them, single or double quotes keep spaces, and a
/// backslash escapes the next character inside double quotes.
pub(crate) fn words(args: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = args.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                started = true;
                let quote = c;
                loop {
                    match chars.next() {
                        Some(end) if end == quote => break,
                        Some('\\') if quote == '"' => {
                            if let Some(next) = chars.next() {
                                word.push(next);
                            }
                        }
                        Some(other) => word.push(other),
                        None => return Err(invalid(format!("a {quote} quote is not closed"))),
                    }
                }
            }
            c if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        out.push(word);
    }
    Ok(out)
}

/// The caller the agent table takes, from how the command reached the kernel: the kernel's
/// socket or pipe is the person at a paired local terminal; HTTP is not present (spec 3.3
/// §7.2, decision 22). Every change records the transport and the calling process.
pub(crate) fn caller(user: &str, reach: Option<&CommandReach>) -> AgentCaller {
    let mut caller = AgentCaller::new(SubjectRef::human(user), "terminal", "keyboard");
    if let Some(reach) = reach {
        caller = caller.with_transport(reach.transport);
        if matches!(reach.transport, ControlTransport::Socket | ControlTransport::Pipe) {
            caller = caller.with_presence(Presence::PairedLocal);
        }
        if let Some(device) = &reach.device {
            caller = caller.with_device(device.clone());
        }
        if let Some(pid) = reach.peer_pid {
            caller = caller.with_peer_pid(pid);
        }
    }
    caller
}

/// The parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Line {
    verb: String,
    target: Option<String>,
    fields: Vec<(String, String)>,
    confirm: Option<String>,
    /// The version an edit or removal was made against: the one the person saw.
    version: Option<u64>,
    json: bool,
}

const FIELDS: &[&str] = &[
    "name",
    "does",
    "instructions",
    "area",
    "asks",
    "model",
    "files",
    "folder",
    "shell",
    "web",
    "pack",
    "driver",
    "limit",
    "colour",
    "color",
];

fn parse(args: &str) -> Result<Line> {
    let mut words = words(args)?.into_iter();
    let mut line = Line { verb: words.next().unwrap_or_else(|| "list".into()), ..Line::default() };
    while let Some(word) = words.next() {
        if word == "--json" {
            line.json = true;
            continue;
        }
        let Some(flag) = word.strip_prefix("--") else {
            if line.target.is_some() {
                return Err(invalid(format!("unexpected `{word}`; quote a value that has spaces")));
            }
            line.target = Some(word);
            continue;
        };
        let (flag, inline) = match flag.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (flag.to_string(), None),
        };
        let value = match inline {
            Some(value) => value,
            None => words.next().ok_or_else(|| invalid(format!("--{flag} needs a value")))?,
        };
        if flag == "confirm" {
            line.confirm = Some(value);
        } else if flag == "version" {
            line.version = Some(value.parse().map_err(|_| {
                invalid(format!("--version is the agent's version as shown, not `{value}`"))
            })?);
        } else if FIELDS.contains(&flag.as_str()) {
            line.fields.push((flag, value));
        } else {
            return Err(invalid(format!("no field --{flag}; /agent help lists them")));
        }
    }
    Ok(line)
}

fn mode_of(value: &str) -> Result<PartitionMode> {
    match value.trim().to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
        "justlook" | "readonly" => Ok(PartitionMode::ReadOnly),
        "suggest" | "plan" => Ok(PartitionMode::Plan),
        "askfirst" | "ask" => Ok(PartitionMode::Ask),
        "goahead" | "auto" => Ok(PartitionMode::Auto),
        _ => Err(invalid(format!(
            "--asks is one of just-look, suggest, ask-first or go-ahead, not `{value}`"
        ))),
    }
}

fn mode_words(mode: PartitionMode) -> &'static str {
    match mode {
        PartitionMode::ReadOnly => "Just looks",
        PartitionMode::Plan => "Suggests",
        PartitionMode::Ask => "Asks first",
        _ => "Goes ahead",
    }
}

/// `definition` with the fields of `line` applied.
fn apply_fields(
    mut definition: AgentDefinition,
    fields: &[(String, String)],
) -> Result<AgentDefinition> {
    let mut packs: Option<Vec<String>> = None;
    let mut drivers: Option<Vec<String>> = None;
    for (field, value) in fields {
        let uses = definition.uses.clone();
        match field.as_str() {
            "name" => definition.display_name = value.clone(),
            "does" => definition.description = value.clone(),
            "instructions" => definition.instructions = value.clone(),
            "area" => definition.area = value.as_str().into(),
            "asks" => definition.mode = mode_of(value)?,
            "model" => {
                let (provider, model) = value.split_once('/').ok_or_else(|| {
                    invalid("--model is provider/model, such as anthropic/claude-sonnet-5-5")
                })?;
                definition.model = Some(ModelChoice::new(provider, model));
            }
            "files" => {
                let files = match value.as_str() {
                    "none" => FileAccess::None,
                    "read" => FileAccess::Read,
                    "read-write" | "readwrite" | "change" => FileAccess::ReadWrite,
                    other => {
                        return Err(invalid(format!(
                            "--files is none, read or read-write, not `{other}`"
                        )));
                    }
                };
                definition.uses = uses.with_files(files);
            }
            "folder" => {
                let mut next = AgentUses::default()
                    .with_files(uses.files)
                    .with_shell(uses.shell)
                    .with_web(uses.web.clone());
                if !value.is_empty() {
                    next = next.with_folder(value.clone());
                }
                for pack in &uses.packs {
                    next = next.with_pack(pack.clone());
                }
                for driver in &uses.drivers {
                    next = next.with_driver(driver.clone());
                }
                definition.uses = next;
            }
            "shell" => {
                let shell = match value.as_str() {
                    "none" => ShellAccess::None,
                    "sandbox" | "sandboxed" => ShellAccess::Sandboxed,
                    "host" | "computer" => ShellAccess::Host,
                    other => {
                        return Err(invalid(format!(
                            "--shell is none, sandbox or host, not `{other}`"
                        )));
                    }
                };
                definition.uses = uses.with_shell(shell);
            }
            "web" => {
                let web = match value.as_str() {
                    "none" => WebAccess::None,
                    "any" => WebAccess::Any,
                    sites => {
                        WebAccess::listed(sites.split(',').map(str::trim).filter(|s| !s.is_empty()))
                    }
                };
                definition.uses = uses.with_web(web);
            }
            "pack" => packs.get_or_insert_with(Vec::new).push(value.clone()),
            "driver" => drivers.get_or_insert_with(Vec::new).push(value.clone()),
            "limit" => {
                definition.daily_limit_micro_usd = if value == "none" {
                    None
                } else {
                    let dollars: f64 = value.trim_start_matches('$').parse().map_err(|_| {
                        invalid(format!("--limit is dollars a day, such as 2.50, not `{value}`"))
                    })?;
                    Some((dollars * 1_000_000.0).round() as u64)
                };
            }
            "colour" | "color" => {
                definition.color = value.parse::<AvatarColor>().map_err(|_| {
                    invalid(format!("--colour is one of the eight avatar colours, not `{value}`"))
                })?;
            }
            _ => {}
        }
    }
    if packs.is_some() || drivers.is_some() {
        let uses = definition.uses.clone();
        let mut next =
            AgentUses::default().with_files(uses.files).with_shell(uses.shell).with_web(uses.web);
        if let Some(folder) = uses.folder {
            next = next.with_folder(folder);
        }
        for pack in packs.unwrap_or(uses.packs) {
            if pack != "none" {
                next = next.with_pack(pack);
            }
        }
        for driver in drivers.unwrap_or(uses.drivers) {
            if driver != "none" {
                next = next.with_driver(driver);
            }
        }
        definition.uses = next;
    }
    Ok(definition)
}

/// The agent `target` names: its id, or its name ignoring case. A removed agent is not one of
/// the person's agents any more.
async fn find(agents: &Agents, target: Option<&str>) -> Result<AgentRecord> {
    let target = target.ok_or_else(|| invalid("name the agent: /agent show Finance"))?;
    let views = agents.list().await.map_err(AdkError::from)?;
    views
        .into_iter()
        .map(|view| view.agent)
        .filter(|record| record.state != AgentRecordState::Removed)
        .find(|record| record.id == target || record.display_name.eq_ignore_ascii_case(target))
        .ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "harness.agent.not_found",
                format!("no agent of yours is called {target}; /agent list lists them"),
            )
        })
}

fn state_word(view: &AgentView) -> &'static str {
    if view.cant_start.is_some() {
        return "Can't start";
    }
    match view.agent.state {
        AgentRecordState::Proposed => "Suggested",
        AgentRecordState::Paused => "Paused",
        AgentRecordState::Removing => "Removing",
        AgentRecordState::Removed => "Removed",
        AgentRecordState::Refused => "Not added",
        _ => "Ready",
    }
}

fn describe(record: &AgentRecord) -> String {
    let mut out = String::new();
    let uses = &record.uses;
    let place = uses.folder.as_deref().unwrap_or("the workspace");
    let _ = writeln!(out, "{}", adk_surface_safe(&record.display_name));
    if !record.description.is_empty() {
        let _ = writeln!(out, "  What it does   {}", adk_surface_safe(&record.description));
    }
    let _ = writeln!(out, "  Area           {}", record.area);
    let _ = writeln!(out, "  Asks           {}", mode_words(record.mode));
    let _ = writeln!(
        out,
        "  Runs on        {}",
        record.model.as_ref().map_or("the primary agent's model".to_string(), |m| m.model.clone())
    );
    let files = match uses.files {
        FileAccess::None => "No files".to_string(),
        FileAccess::Read => format!("Reads files in {place}"),
        FileAccess::ReadWrite => format!("Reads and changes files in {place}"),
    };
    let _ = writeln!(out, "  Files          {files}");
    let shell = match uses.shell {
        ShellAccess::None => "No shell",
        ShellAccess::Sandboxed => "In the sandbox",
        ShellAccess::Host => "On this computer",
    };
    let _ = writeln!(out, "  Shell          {shell}");
    let web = match &uses.web {
        WebAccess::None => "No web".to_string(),
        WebAccess::Listed { hosts } => format!("Listed sites only: {}", hosts.join(", ")),
        WebAccess::Any => "Any site".to_string(),
    };
    let _ = writeln!(out, "  Web            {web}");
    if !uses.packs.is_empty() {
        let _ = writeln!(out, "  Packs          {}", uses.packs.join(", "));
    }
    if !uses.drivers.is_empty() {
        let _ = writeln!(out, "  Drivers        {}", uses.drivers.join(", "));
    }
    if let Some(limit) = record.daily_limit_micro_usd {
        let _ = writeln!(out, "  Daily limit    ${:.2}", limit as f64 / 1_000_000.0);
    }
    if !record.instructions.is_empty() {
        let _ = writeln!(out, "  Instructions");
        for line in record.instructions.lines() {
            let _ = writeln!(out, "    > {}", adk_surface_safe(line));
        }
    }
    out
}

/// Every control character but newline and tab, as the terminal escapes it (S-I9).
fn adk_surface_safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            let unsafe_char = (c.is_control() && c != '\n' && c != '\t')
                || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}');
            if unsafe_char {
                format!("\\u{{{:x}}}", c as u32).chars().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// One field of a definition in the words of "Here is what Finance can do": its label and its
/// value as the person reads it.
fn field_words(field: &str, definition: &AgentDefinition) -> (&'static str, String) {
    let uses = &definition.uses;
    match field {
        "displayName" => ("Name", definition.display_name.clone()),
        "color" => ("Colour", definition.color.to_string()),
        "description" => ("What it does", definition.description.clone()),
        "instructions" => ("Instructions", definition.instructions.clone()),
        "area" => ("Area", definition.area.to_string()),
        "mode" => (
            "How much it asks first",
            match definition.mode {
                PartitionMode::ReadOnly => "Just look",
                PartitionMode::Plan => "Suggest",
                PartitionMode::Ask => "Ask first",
                _ => "Go ahead",
            }
            .to_string(),
        ),
        "model" => (
            "Runs on",
            definition
                .model
                .as_ref()
                .map_or("the primary agent's model".to_string(), |model| model.model.clone()),
        ),
        "dailyLimitMicroUsd" => (
            "Daily spending limit",
            definition.daily_limit_micro_usd.map_or("the area's".to_string(), |limit| {
                format!("${:.2}", limit as f64 / 1_000_000.0)
            }),
        ),
        _ => {
            let place = uses.folder.as_deref().unwrap_or("the workspace");
            let files = match uses.files {
                FileAccess::None => "no files".to_string(),
                FileAccess::Read => format!("reads files in {place}"),
                FileAccess::ReadWrite => format!("reads and changes files in {place}"),
            };
            let shell = match uses.shell {
                ShellAccess::None => "no shell",
                ShellAccess::Sandboxed => "the shell in the sandbox",
                ShellAccess::Host => "a shell on this computer",
            };
            let web = match &uses.web {
                WebAccess::None => "no web".to_string(),
                WebAccess::Listed { hosts } => format!("listed sites only ({})", hosts.join(", ")),
                WebAccess::Any => "any site".to_string(),
            };
            let mut parts = vec![files, shell.to_string(), web];
            if !uses.packs.is_empty() {
                parts.push(format!("packs {}", uses.packs.join(", ")));
            }
            if !uses.drivers.is_empty() {
                parts.push(format!("drivers {}", uses.drivers.join(", ")));
            }
            ("What it can use", parts.join("; "))
        }
    }
}

/// What a change does, as the person reads it before confirming (spec 3.3 §5.7): **Here is
/// what Finance can do** with each changed field as "Was … Now …" and the risk, or for a
/// removal the sentence naming what ends.
pub(crate) fn preview_words(preview: &AgentPreview, name: &str) -> String {
    let mut out = String::new();
    if let Some(ended) = &preview.ended {
        let count = |n: usize, one: &str, many: &str| {
            if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
        };
        let mut ends = Vec::new();
        if !ended.decisions.is_empty() {
            ends.push(format!(
                "{} waiting for you",
                count(ended.decisions.len(), "decision", "decisions")
            ));
        }
        if !ended.routines.is_empty() {
            ends.push(count(ended.routines.len(), "routine", "routines"));
        }
        if !ended.follow_ups.is_empty() {
            ends.push(count(ended.follow_ups.len(), "follow-up", "follow-ups"));
        }
        let ends = match ends.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        };
        let work = ended.work.len();
        let body = match (work, ends.is_empty()) {
            (0, true) => format!("Removing {name} ends nothing that is running"),
            (0, false) => format!("Removing {name} ends {ends}"),
            (n, true) => {
                format!("Removing {name} stops {}", count(n, "piece of work", "pieces of work"))
            }
            (n, false) => format!(
                "Removing {name} stops {}, ends {ends}",
                count(n, "piece of work", "pieces of work")
            ),
        };
        let _ = writeln!(out, "{body}. Its threads stay.");
        return out;
    }
    let _ = writeln!(out, "Here is what {name} can do:");
    for ability in &preview.abilities {
        let _ = writeln!(out, "  - {}", adk_surface_safe(ability));
    }
    if let (Some(was), Some(now)) = (&preview.was, &preview.now) {
        for field in &preview.fields {
            let (label, before) = field_words(field, was);
            let (_, after) = field_words(field, now);
            let _ = writeln!(
                out,
                "  {label}: Was {} · Now {}",
                adk_surface_safe(&before),
                adk_surface_safe(&after)
            );
        }
    }
    let risk = match preview.risk {
        adk_core::ApprovalRisk::Low => "Low risk",
        adk_core::ApprovalRisk::Medium => "Medium risk",
        _ => "High risk",
    };
    let money = if preview.money { " · spends money" } else { "" };
    let _ = writeln!(out, "{risk}{money} · You can remove it later");
    if preview.needs_presence() {
        let _ = writeln!(
            out,
            "High risk: confirm it on the kernel's own terminal, or in the web home after confirming it's you."
        );
    }
    out
}

fn preview_text(preview: &AgentPreview, name: &str, confirm_line: &str) -> String {
    let mut out = preview_words(preview, name);
    let _ = write!(out, "To confirm: {confirm_line} --confirm {}", preview.code());
    out
}

/// Runs `/agent args` for `user` on the kernel `agents` is bound to, reached as `reach`.
///
/// # Errors
///
/// Returns an invalid-input error for a command it cannot read, `not_found` for an agent the
/// person does not have, or the agent table's refusal.
pub(crate) async fn run(
    agents: Option<&AgentsHandle>,
    user: &str,
    reach: Option<&CommandReach>,
    args: &str,
) -> Result<String> {
    let Some(agents) = agents.and_then(AgentsHandle::get) else {
        return Ok(NO_KERNEL.to_string());
    };
    let line = parse(args)?;
    let caller = caller(user, reach);
    match line.verb.as_str() {
        "list" | "ls" => {
            let views = agents.list().await.map_err(AdkError::from)?;
            // The JSON carries every agent with its state, removed ones included, for the
            // Agents view's "Removed agents" (spec 3.3 §5.7); the words list the person's own.
            if line.json {
                return Ok(json!({ "agents": views }).to_string());
            }
            let views: Vec<AgentView> = views
                .into_iter()
                .filter(|view| view.agent.state != AgentRecordState::Removed)
                .collect();
            if views.is_empty() {
                return Ok(
                    "No agents of your own yet. Add one with /agent new --name NAME, or ask ADK-Rust to suggest one."
                        .into(),
                );
            }
            let mut out = String::new();
            for view in &views {
                let _ = writeln!(
                    out,
                    "  {:<24} {:<12} {:<10} {}",
                    adk_surface_safe(&view.agent.display_name),
                    state_word(view),
                    view.agent.area,
                    mode_words(view.agent.mode)
                );
                if let Some(reason) = &view.cant_start {
                    let _ = writeln!(out, "      {}", adk_surface_safe(reason));
                }
            }
            Ok(out.trim_end().to_string())
        }
        "show" => {
            let record = find(&agents, line.target.as_deref()).await?;
            let view = agents.get(&record.id).await.map_err(AdkError::from)?;
            if line.json {
                return Ok(json!({ "agent": view }).to_string());
            }
            let mut out = describe(&record);
            if let Some(reason) = view.and_then(|view| view.cant_start) {
                let _ = writeln!(out, "  Can't start    {}", adk_surface_safe(&reason));
            }
            Ok(out.trim_end().to_string())
        }
        "new" | "add" => {
            if !line.fields.iter().any(|(field, _)| field == "name") {
                return Err(invalid("give it a name: /agent new --name Finance"));
            }
            let mut definition = apply_fields(AgentDefinition::new("", "default"), &line.fields)?;
            if !line.fields.iter().any(|(field, _)| field == "colour" || field == "color") {
                definition.color = AvatarColor::for_name(&definition.display_name);
            }
            let name = definition.display_name.clone();
            let action = AgentAction::Create { definition, replaces: None };
            confirm_or_preview(
                &agents,
                &caller,
                action,
                &line,
                &name,
                &format!("/agent {}", args_without_confirm(args)),
            )
            .await
        }
        "edit" | "change" => {
            let record = find(&agents, line.target.as_deref()).await?;
            let definition = apply_fields(record.definition(), &line.fields)?;
            let name = definition.display_name.clone();
            // Against the version the person saw: a change made since is not overwritten
            // (spec 3.3 §5.7).
            let action = AgentAction::Change {
                id: record.id.clone(),
                expected_version: line.version.unwrap_or(record.version),
                definition,
            };
            confirm_or_preview(
                &agents,
                &caller,
                action,
                &line,
                &name,
                &format!("/agent {}", args_without_confirm(args)),
            )
            .await
        }
        "pause" | "resume" | "remove" | "rm" => {
            let record = find(&agents, line.target.as_deref()).await?;
            let action = match line.verb.as_str() {
                "pause" => AgentAction::Pause { id: record.id.clone() },
                "resume" => AgentAction::Resume { id: record.id.clone() },
                _ => AgentAction::Remove {
                    id: record.id.clone(),
                    expected_version: line.version.unwrap_or(record.version),
                },
            };
            let name = record.display_name.clone();
            confirm_or_preview(
                &agents,
                &caller,
                action,
                &line,
                &name,
                &format!("/agent {}", args_without_confirm(args)),
            )
            .await
        }
        "help" => Ok(format!(
            "{DESCRIPTION}\nFields: --name, --does, --instructions, --area, --asks just-look|suggest|ask-first|go-ahead, \
             --model provider/model, --files none|read|read-write, --folder DIR, --shell none|sandbox|host, \
             --web none|any|site,site, --pack NAME, --driver NAME, --limit DOLLARS, --colour NAME.\n\
             A change prints what it does and the line that confirms it."
        )),
        other => Err(invalid(format!("no /agent {other}; /agent help lists the verbs"))),
    }
}

/// `args` without its `--confirm` and its code.
fn args_without_confirm(args: &str) -> String {
    let words = words(args).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut skip = false;
    for word in words {
        if skip {
            skip = false;
            continue;
        }
        if word == "--confirm" {
            skip = true;
            continue;
        }
        if word.starts_with("--confirm=") || word == "--json" {
            continue;
        }
        out.push(if word.is_empty() || word.contains(char::is_whitespace) || word.contains('"') {
            format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
        } else {
            word
        });
    }
    out.join(" ")
}

async fn confirm_or_preview(
    agents: &Agents,
    caller: &AgentCaller,
    action: AgentAction,
    line: &Line,
    name: &str,
    confirm_line: &str,
) -> Result<String> {
    let preview = agents.preview(caller, &action).await.map_err(AdkError::from)?;
    let Some(code) = &line.confirm else {
        if line.json {
            return Ok(json!({
                "preview": preview,
                "code": preview.code(),
                "name": name,
                "text": preview_words(&preview, &adk_surface_safe(name)),
            })
            .to_string());
        }
        return Ok(preview_text(&preview, name, confirm_line));
    };
    if code.trim() != preview.code() {
        return Err(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Conflict,
            "harness.agent.unconfirmed",
            format!(
                "{code} does not confirm this change: it changed since it was shown, or the code is \
                 another change's. Run it without --confirm to see it again."
            ),
        ));
    }
    let outcome = agents.apply(caller, action, &preview.digest).await.map_err(AdkError::from)?;
    if line.json {
        return Ok(json!({ "outcome": outcome }).to_string());
    }
    Ok(match outcome {
        adk_kernel::agents::AgentOutcome::Created { agent } => {
            format!(
                "Added {}. It is ready; /agent show {} shows it.",
                adk_surface_safe(&agent.display_name),
                agent.id
            )
        }
        adk_kernel::agents::AgentOutcome::Changed { agent } => {
            format!("Saved {}.", adk_surface_safe(&agent.display_name))
        }
        adk_kernel::agents::AgentOutcome::Paused { agent } => {
            format!("{} is paused.", adk_surface_safe(&agent.display_name))
        }
        adk_kernel::agents::AgentOutcome::Resumed { agent } => {
            format!("{} runs again.", adk_surface_safe(&agent.display_name))
        }
        adk_kernel::agents::AgentOutcome::Removed { agent, .. } => {
            format!("Removed {}. Its threads stay.", adk_surface_safe(&agent.display_name))
        }
        other => format!("{other:?}"),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn words_keep_quoted_spaces() {
        assert_eq!(
            words(r#"new --name "Finance team" --does 'reads it' --web a.io,b.io"#).unwrap(),
            ["new", "--name", "Finance team", "--does", "reads it", "--web", "a.io,b.io"]
        );
        assert_eq!(
            words(r#"--instructions "say \"hi\"""#).unwrap(),
            ["--instructions", r#"say "hi""#]
        );
        assert!(words(r#"--name "open"#).is_err());
        assert_eq!(words("  ").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_line_has_a_verb_a_target_fields_and_a_code() {
        let line = parse("edit finance --asks suggest --confirm 3f9a2c --json").unwrap();
        assert_eq!(line.verb, "edit");
        assert_eq!(line.target.as_deref(), Some("finance"));
        assert_eq!(line.fields, [("asks".to_string(), "suggest".to_string())]);
        assert_eq!(line.confirm.as_deref(), Some("3f9a2c"));
        assert!(line.json);
        assert_eq!(parse("").unwrap().verb, "list");
        assert!(parse("new --nonesuch x").is_err());
        assert!(parse("new --name").is_err());
    }

    #[test]
    fn fields_lower_onto_the_definition() {
        let definition = apply_fields(
            AgentDefinition::new("", "default"),
            &[
                ("name".into(), "Finance".into()),
                ("asks".into(), "ask-first".into()),
                ("files".into(), "read".into()),
                ("folder".into(), "Invoices".into()),
                ("shell".into(), "sandbox".into()),
                ("web".into(), "bank.example".into()),
                ("pack".into(), "browser".into()),
                ("limit".into(), "2.50".into()),
                ("model".into(), "anthropic/claude-sonnet-5-5".into()),
                ("colour".into(), "teal".into()),
            ],
        )
        .unwrap();
        assert_eq!(definition.display_name, "Finance");
        assert_eq!(definition.mode, PartitionMode::Ask);
        assert_eq!(definition.uses.files, FileAccess::Read);
        assert_eq!(definition.uses.folder.as_deref(), Some("Invoices"));
        assert_eq!(definition.uses.shell, ShellAccess::Sandboxed);
        assert_eq!(definition.uses.web, WebAccess::listed(["bank.example"]));
        assert_eq!(definition.uses.packs, ["browser"]);
        assert_eq!(definition.daily_limit_micro_usd, Some(2_500_000));
        assert_eq!(definition.model, Some(ModelChoice::new("anthropic", "claude-sonnet-5-5")));
        assert_eq!(definition.color, AvatarColor::Teal);
        assert!(
            apply_fields(AgentDefinition::new("", "default"), &[("asks".into(), "always".into())])
                .is_err()
        );
    }

    #[test]
    fn the_socket_and_the_pipe_are_the_person_at_the_kernel() {
        let socket =
            caller("ana", Some(&CommandReach::new(ControlTransport::Socket).with_peer_pid(42)));
        assert_eq!(socket.presence, Presence::PairedLocal);
        assert_eq!(socket.transport, Some(ControlTransport::Socket));
        assert_eq!(socket.peer_pid, Some(42));
        let http = caller("ana", Some(&CommandReach::new(ControlTransport::Http)));
        assert_eq!(http.presence, Presence::None);
        let device =
            caller("ana", Some(&CommandReach::new(ControlTransport::Http).with_device("phone-1")));
        assert!(device.refusal().is_some(), "a paired device changes no agent");
        assert_eq!(caller("ana", None).presence, Presence::None);
    }

    #[test]
    fn the_confirming_line_drops_the_code() {
        assert_eq!(
            args_without_confirm(r#"new --name "Finance team" --confirm abc123 --json"#),
            r#"new --name "Finance team""#
        );
    }

    /// A kernel whose agents are kept but never built: a change is all these tests need.
    fn kernel() -> (adk_kernel::Kernel, AgentsHandle) {
        struct NoBuild;

        #[async_trait::async_trait]
        impl adk_kernel::agents::AgentFactory for NoBuild {
            fn assess(&self, _record: &AgentRecord) -> adk_kernel::agents::AgentAssessment {
                adk_kernel::agents::AgentAssessment::new(adk_core::ApprovalRisk::Low)
            }
            async fn ceiling(
                &self,
                _agent: &adk_core::AgentId,
                _session: &str,
            ) -> adk_kernel::Result<Option<adk_kernel::agents::AgentCeiling>> {
                Ok(None)
            }
            async fn choices(&self) -> adk_kernel::Result<adk_kernel::agents::AgentChoices> {
                Ok(adk_kernel::agents::AgentChoices::default())
            }
            async fn build(
                &self,
                _record: &AgentRecord,
            ) -> adk_kernel::Result<adk_kernel::agents::HostedAgent> {
                Err(adk_kernel::KernelError::Config { reason: "not built in this test".into() })
            }
            async fn retired(&self, _record: &AgentRecord) -> adk_kernel::Result<u64> {
                Ok(0)
            }
        }

        let handle = AgentsHandle::new();
        let kernel =
            adk_kernel::Kernel::builder()
                .config(adk_kernel::KernelConfig::default().with_app_name("adk-rust").with_control(
                    adk_kernel::control::ControlPlaneConfig::default().without_socket(),
                ))
                .run_store(std::sync::Arc::new(adk_core::InMemoryRunStore::new()))
                .agent_factory(std::sync::Arc::new(NoBuild))
                .agents_handle(handle.clone())
                .build()
                .unwrap();
        (kernel, handle)
    }

    /// `/agent args` previewed, then confirmed with its code, as `reach`.
    async fn confirmed(handle: &AgentsHandle, reach: &CommandReach, args: &str) -> Result<String> {
        let preview = run(Some(handle), "ana", Some(reach), &format!("{args} --json")).await?;
        let preview: Value = serde_json::from_str(&preview).unwrap();
        let code = preview["code"].as_str().unwrap().to_string();
        run(Some(handle), "ana", Some(reach), &format!("{args} --confirm {code}")).await
    }

    /// Wave 3 findings 14 and 22: an edit is made against the version the person saw, so one
    /// made since is never overwritten; the preview shows each change as "Was … Now …".
    #[tokio::test]
    async fn an_edit_is_made_against_the_version_the_person_saw() {
        let (_kernel, handle) = kernel();
        let local = CommandReach::new(ControlTransport::Socket);
        confirmed(&handle, &local, "new --name Finance --asks ask-first").await.unwrap();
        confirmed(&handle, &local, r#"edit finance --instructions "Draft only""#).await.unwrap();
        let stale =
            run(Some(&handle), "ana", Some(&local), "edit finance --version 1 --asks suggest")
                .await
                .unwrap_err();
        assert_eq!(stale.code, "kernel.agent.out_of_date", "{stale}");
        let preview =
            run(Some(&handle), "ana", Some(&local), "edit finance --version 2 --asks suggest")
                .await
                .unwrap();
        assert!(
            preview.contains("How much it asks first: Was Ask first · Now Suggest"),
            "{preview}"
        );
        let json = run(Some(&handle), "ana", Some(&local), "remove finance --json").await.unwrap();
        let json: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            json["text"],
            "Removing Finance ends nothing that is running. Its threads stay.\n"
        );
    }

    /// Wave 3 findings 17–19: a terminal over HTTP pauses an agent but never resumes one;
    /// the kernel's own terminal does.
    #[tokio::test]
    async fn resume_over_http_is_refused() {
        let (_kernel, handle) = kernel();
        let local = CommandReach::new(ControlTransport::Socket);
        let http = CommandReach::new(ControlTransport::Http);
        confirmed(&handle, &local, "new --name Finance --asks suggest").await.unwrap();
        confirmed(&handle, &http, "pause finance").await.unwrap();
        let error = confirmed(&handle, &http, "resume finance").await.unwrap_err();
        assert_eq!(error.code, "kernel.agent.refused", "{error}");
        assert!(confirmed(&handle, &local, "resume finance").await.unwrap().contains("runs again"));
    }

    #[tokio::test]
    async fn without_a_kernel_it_says_to_start_one() {
        assert_eq!(run(None, "ana", None, "list").await.unwrap(), NO_KERNEL);
        assert_eq!(
            run(Some(&AgentsHandle::new()), "ana", None, "new --name X").await.unwrap(),
            NO_KERNEL
        );
    }
}
