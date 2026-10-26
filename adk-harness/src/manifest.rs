//! What the harness can do, generated from the running harness (spec 3.1 §7.25).
//!
//! The model judges a harness by what it is told about it. [`HarnessManifest`] is built
//! once, when the harness is built, from the class registry, the mode table under the
//! build's switches, the packs and the surface's commands; [`HarnessManifest::render`]
//! turns it into the `<harness>` block of the system instruction, and `harness_help`
//! returns the detail on demand.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::mode::HarnessMode;
use crate::policy::{PolicySwitches, TableDecision, TableRow, decision_cell};
use crate::web::WebSearchPath;

/// The most bytes the `<harness>` block takes in the system instruction (spec 3.1 §7.25).
pub const HARNESS_BLOCK_MAX_BYTES: usize = 6 * 1024;

/// The most bytes one `harness_help` answer returns (spec 3.1 §7.25).
pub const HARNESS_HELP_MAX_BYTES: usize = 32 * 1024;

/// What this harness can do, generated when it is built (spec 3.1 §7.25).
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessAgent, InstructionsConfig};
/// use adk_model::{ScriptedLlm, ScriptedResponse};
/// use std::sync::Arc;
///
/// let dir = tempfile::tempdir().unwrap();
/// let harness = HarnessAgent::builder("assistant")
///     .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")])))
///     .workspace(adk_devtools::Workspace::new(dir.path()))
///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
///     .build()?;
/// let manifest = harness.manifest();
/// assert!(manifest.classes.iter().any(|class| class.tools.iter().any(|tool| tool == "harness_help")));
/// assert!(manifest.render().starts_with("<harness version="));
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct HarnessManifest {
    /// adk-harness's version.
    pub harness_version: String,
    /// The surface the person uses: its commands, and whether the kernel runs.
    pub surface: SurfaceManifest,
    /// The mode of sessions with no mode record.
    pub default_mode: HarnessMode,
    /// Every class this harness declares tools in, with what each mode does with it.
    pub classes: Vec<ClassEntry>,
    /// Team members.
    pub members: Vec<MemberEntry>,
    /// Packs: active, compiled in and off, or not compiled.
    pub packs: Vec<PackEntry>,
    /// The path `web_search` takes (spec 3.1 §7.20).
    pub web: WebSearchPath,
    /// Working notes and long-term facts (spec 3.1 §7.15).
    pub memory: MemoryEntry,
    /// What each safety mechanism covers in this build.
    pub safety: Vec<SafetyEntry>,
    /// Languages with a language server (spec 3.1 §7.28); empty until servers exist.
    pub languages: Vec<LanguageEntry>,
    /// The edit tool recommended for changes across files for this harness's model, when
    /// it declares edit tools (spec 3.1 §7.26, §13 Q12).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_file_edit: Option<String>,
    /// How background shells behave under this build's isolation, when `bash` runs them
    /// (spec 3.1 §7.29).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_shells: Option<String>,
    /// What `harness_help` can read.
    pub help_topics: Vec<String>,
}

/// One class of call and what each mode does with it (spec 3.1 §7.25).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ClassEntry {
    /// The class, as the block names it (`read`, `edit`, `shell (host)`).
    pub class: String,
    /// The tools of the class this harness declares, sorted.
    pub tools: Vec<String>,
    /// What a call of the class gets in ReadOnly, Plan, Ask and Auto.
    pub decisions: [ModeDecision; 4],
    /// What the class covers beyond its tools, such as the protected paths of the
    /// protected-edit class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What a class gets in a mode (spec 3.1 §7.25).
///
/// # Example
///
/// ```rust
/// use adk_harness::ModeDecision;
///
/// assert_eq!(ModeDecision::AskEveryTime.label(), "ask every time");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ModeDecision {
    /// Runs without asking.
    Allow,
    /// The person is asked; a standing rule may allow or deny it.
    Ask,
    /// The person is asked every time; no standing rule applies.
    AskEveryTime,
    /// Refused.
    Deny,
}

impl ModeDecision {
    /// The decision as the block writes it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::AskEveryTime => "ask every time",
            Self::Deny => "deny",
        }
    }

    fn of(decision: TableDecision) -> Self {
        match decision {
            TableDecision::Allow => Self::Allow,
            TableDecision::Deny => Self::Deny,
            TableDecision::RequireApproval { non_waivable: false } => Self::Ask,
            TableDecision::RequireApproval { non_waivable: true } => Self::AskEveryTime,
        }
    }
}

/// What the person uses: the surface's commands, and the kernel (spec 3.1 §7.25).
///
/// A surface (adk-cli, `serve`, the ACP server) fills it from its own command tables
/// and passes it to [`HarnessAgentBuilder::surface`](crate::HarnessAgentBuilder::surface);
/// a harness built without one reports [`SurfaceManifest::library`].
///
/// # Example
///
/// ```rust
/// use adk_harness::{CommandEntry, SurfaceManifest};
///
/// let surface = SurfaceManifest::new("adk-rust", "3.0.0")
///     .command(CommandEntry::slash("approvals", "Calls in this session waiting for a decision"));
/// assert_eq!(surface.commands.len(), 1);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SurfaceManifest {
    /// `adk-rust`, `adk-rust serve`, `acp` or `library`.
    pub name: String,
    /// The surface's version.
    pub version: String,
    /// What a person types: slash commands, subcommands and flags.
    pub commands: Vec<CommandEntry>,
    /// The kernel: compiled in, and whether this session runs under it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<KernelEntry>,
    /// How the person turns on each pack that is off, by pack name (`--pack browser`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pack_switches: BTreeMap<String, String>,
}

impl SurfaceManifest {
    /// A surface with no commands yet.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            commands: Vec::new(),
            kernel: None,
            pack_switches: BTreeMap::new(),
        }
    }

    /// The manifest of a harness built as a library, with no surface commands.
    ///
    /// # Example
    ///
    /// ```rust
    /// assert_eq!(adk_harness::SurfaceManifest::library().name, "library");
    /// ```
    pub fn library() -> Self {
        Self::new("library", env!("CARGO_PKG_VERSION"))
    }

    /// Adds `entry`.
    #[must_use]
    pub fn command(mut self, entry: CommandEntry) -> Self {
        self.commands.push(entry);
        self
    }

    /// Adds `entries`.
    #[must_use]
    pub fn commands(mut self, entries: impl IntoIterator<Item = CommandEntry>) -> Self {
        self.commands.extend(entries);
        self
    }

    /// Records the kernel's state.
    #[must_use]
    pub fn with_kernel(mut self, kernel: KernelEntry) -> Self {
        self.kernel = Some(kernel);
        self
    }

    /// Records how the person turns on `pack` when it is off.
    #[must_use]
    pub fn pack_switch(mut self, pack: impl Into<String>, how: impl Into<String>) -> Self {
        self.pack_switches.insert(pack.into(), how.into());
        self
    }
}

/// A command a person types (spec 3.1 §7.25).
///
/// # Example
///
/// ```rust
/// use adk_harness::{CommandEntry, CommandKind};
///
/// let entry = CommandEntry::subcommand("up", "Run the harness as the agent kernel", None);
/// assert_eq!(entry.kind, CommandKind::Subcommand);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CommandEntry {
    /// Slash command, subcommand or flag.
    pub kind: CommandKind,
    /// The command as typed, without its `/` or its parent command.
    pub name: String,
    /// One line on what it does.
    pub summary: String,
    /// The long help `harness_help cli <subcommand>` returns, or the usage line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
}

impl CommandEntry {
    /// A slash command of the REPL.
    pub fn slash(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self { kind: CommandKind::Slash, name: name.into(), summary: summary.into(), usage: None }
    }

    /// A subcommand of the binary, with its long help.
    pub fn subcommand(
        name: impl Into<String>,
        summary: impl Into<String>,
        long_help: Option<String>,
    ) -> Self {
        Self {
            kind: CommandKind::Subcommand,
            name: name.into(),
            summary: summary.into(),
            usage: long_help,
        }
    }

    /// A flag, written as typed (`--continue`, `rules add --deny`).
    pub fn flag(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self { kind: CommandKind::Flag, name: name.into(), summary: summary.into(), usage: None }
    }

    fn typed(&self, surface: &str) -> String {
        match self.kind {
            CommandKind::Slash => format!("/{}", self.name),
            CommandKind::Subcommand => format!("{surface} {}", self.name),
            CommandKind::Flag => format!("{surface} {}", self.name),
        }
    }
}

/// The kind of a [`CommandEntry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandKind {
    /// Typed in the REPL after `/`.
    Slash,
    /// A subcommand of the binary.
    Subcommand,
    /// A flag of the binary or a subcommand.
    Flag,
}

/// The kernel as the surface sees it (spec 3.1 §7.25, spec 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KernelEntry {
    /// The surface was built with the kernel.
    pub compiled: bool,
    /// This session runs under a kernel.
    pub running: bool,
}

impl KernelEntry {
    /// A kernel entry.
    pub fn new(compiled: bool, running: bool) -> Self {
        Self { compiled, running }
    }
}

/// A team member (spec 3.1 §7.10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MemberEntry {
    /// The member's name, which is also its delegation tool.
    pub name: String,
    /// `local`, `acp`, `a2a` or `agent`.
    pub kind: String,
    /// `read-only`, `full` or `custom`.
    pub tools: String,
    /// The workspace the member edits (`shared` until spec 3.1 §7.31).
    pub workspace: String,
}

impl MemberEntry {
    /// A member entry.
    pub fn new(name: impl Into<String>, kind: impl Into<String>, tools: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            tools: tools.into(),
            workspace: "shared".into(),
        }
    }
}

/// A pack (spec 3.1 §7.16–§7.18, §7.24).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PackEntry {
    /// `browser`, `computer-use`, `payments`, `voice` or `managed`.
    pub name: String,
    /// Whether it is active.
    pub state: PackState,
    /// How the person turns it on, when it is off and the surface says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable: Option<String>,
}

/// Whether a pack is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PackState {
    /// Active in this harness.
    Active,
    /// Compiled in and off.
    Off,
    /// Not compiled into this build.
    NotCompiled,
}

/// Working notes and long-term facts (spec 3.1 §7.15).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MemoryEntry {
    /// `memory_read`, `memory_write` and `memory_delete` over working notes.
    pub working_notes: bool,
    /// `memory_search` and `memory_remember` over long-term facts.
    pub long_term_facts: bool,
    /// What embeds facts for recall: `lexical` without an embedder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedder: Option<String>,
}

/// What one safety mechanism covers in this build (spec 3.1 §7.25).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SafetyEntry {
    /// The mechanism.
    pub mechanism: String,
    /// What it covers.
    pub covers: String,
    /// What it does not cover.
    pub does_not_cover: String,
}

impl SafetyEntry {
    fn new(mechanism: &str, covers: &str, does_not_cover: &str) -> Self {
        Self {
            mechanism: mechanism.into(),
            covers: covers.into(),
            does_not_cover: does_not_cover.into(),
        }
    }
}

/// A language with a language server (spec 3.1 §7.28).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct LanguageEntry {
    /// File extensions.
    pub extensions: Vec<String>,
    /// The server.
    pub server: String,
    /// Whether it runs.
    pub running: bool,
}

/// What the build knows when it generates the manifest.
pub(crate) struct ManifestInputs<'a> {
    pub(crate) surface: SurfaceManifest,
    pub(crate) default_mode: HarnessMode,
    /// Every declared tool with its row, before grouping.
    pub(crate) rows: Vec<(String, TableRow)>,
    pub(crate) switches: PolicySwitches,
    pub(crate) web_allowlist: &'a [String],
    pub(crate) shell: Option<&'a crate::shell::IsolationStatus>,
    pub(crate) unsandboxed_retry: bool,
    pub(crate) members: Vec<MemberEntry>,
    pub(crate) active_packs: &'a [String],
    pub(crate) web: WebSearchPath,
    pub(crate) memory: MemoryEntry,
    pub(crate) ledger_durable: bool,
    pub(crate) plan_rewind: bool,
    /// The snapshot configuration, when the harness takes snapshots (spec 3.1 §7.27).
    pub(crate) snapshots: Option<crate::snapshot::SnapshotConfig>,
    /// The harness model's name, for the preferred multi-file edit tool.
    pub(crate) model: &'a str,
}

/// Every pack and whether this build compiles it.
fn compiled_packs() -> [(&'static str, bool); 5] {
    [
        ("browser", cfg!(feature = "browser")),
        ("computer-use", cfg!(feature = "computer-use")),
        ("managed", cfg!(feature = "managed")),
        ("payments", cfg!(feature = "payments")),
        ("voice", cfg!(feature = "voice")),
    ]
}

fn row_name(row: TableRow) -> &'static str {
    match row {
        TableRow::ReadTodoDelegate => "read, todos and members",
        TableRow::PlanExit => "plan exit",
        TableRow::Edit => "edit",
        TableRow::EditProtected => "edit, protected path",
        TableRow::ShellOsSandbox => "shell (OS sandbox)",
        TableRow::ShellContainer => "shell (container)",
        TableRow::ShellHost => "shell (host)",
        TableRow::ShellEscalation => "shell, unsandboxed retry",
        TableRow::MemoryWrite => "memory write",
        TableRow::MemoryPropose => "memory, assumed facts",
        TableRow::WebAllowlisted => "web, allowlisted host",
        TableRow::WebOther => "web, other host",
        TableRow::Payment => "payment",
        TableRow::ComputerUse => "desktop action",
        TableRow::ExternalReadOnlyTrusted => "other, read-only from a trusted publisher",
        TableRow::ExternalOther => "other (MCP servers, drivers, skills, extras)",
    }
}

fn decisions(row: TableRow, switches: &PolicySwitches) -> [ModeDecision; 4] {
    HarnessMode::ALL.map(|mode| ModeDecision::of(decision_cell(mode, row).resolve(switches)))
}

impl HarnessManifest {
    pub(crate) fn generate(inputs: ManifestInputs<'_>) -> Self {
        // Rows in table order, each with its tools and an optional note.
        let mut rows: BTreeMap<usize, (TableRow, Vec<String>, Option<String>)> = BTreeMap::new();
        let position = |row: TableRow| TableRow::ALL.iter().position(|r| *r == row).unwrap_or(0);
        for (tool, row) in &inputs.rows {
            rows.entry(position(*row)).or_insert((*row, Vec::new(), None)).1.push(tool.clone());
        }
        let mut add = |row: TableRow, note: Option<String>| {
            rows.entry(position(row)).or_insert((row, Vec::new(), None)).2 = note;
        };
        if inputs.rows.iter().any(|(_, row)| *row == TableRow::Edit) {
            add(
                TableRow::EditProtected,
                Some(
                    "AGENTS.md, CLAUDE.md, MEMORY.md, .envrc, lefthook.yml and anything under \
                     .git, .adk, .skills or .claude"
                        .into(),
                ),
            );
        }
        if inputs.unsandboxed_retry {
            add(TableRow::ShellEscalation, Some("bash with unsandboxed: true".into()));
        }
        if inputs.rows.iter().any(|(_, row)| *row == TableRow::WebOther)
            && !inputs.web_allowlist.is_empty()
        {
            add(TableRow::WebAllowlisted, Some(inputs.web_allowlist.join(", ")));
        }
        // Tools that toolsets declare per request: MCP servers, drivers and skills.
        add(TableRow::ExternalOther, None);
        let classes = rows
            .into_values()
            .map(|(row, mut tools, note)| {
                tools.sort();
                tools.dedup();
                ClassEntry {
                    class: row_name(row).to_string(),
                    tools,
                    decisions: decisions(row, &inputs.switches),
                    note,
                }
            })
            .collect();
        let packs = compiled_packs()
            .into_iter()
            .map(|(name, compiled)| {
                let state = if inputs.active_packs.iter().any(|pack| pack == name) {
                    PackState::Active
                } else if compiled {
                    PackState::Off
                } else {
                    PackState::NotCompiled
                };
                PackEntry {
                    name: name.to_string(),
                    state,
                    enable: (state == PackState::Off)
                        .then(|| inputs.surface.pack_switches.get(name).cloned())
                        .flatten(),
                }
            })
            .collect();
        let mut safety = vec![SafetyEntry::new(
            "action ledger",
            "a tool call with side effects that already ran is answered from its record, not run \
             again, when a run resumes",
            if inputs.ledger_durable {
                "effects of commands the shell runs"
            } else {
                "effects of commands the shell runs; the ledger lives in memory, so a restart forgets it"
            },
        )];
        if let Some(shell) = inputs.shell {
            safety.push(sandbox_entry(shell));
        }
        safety.push(match &inputs.snapshots {
            // What a snapshot covers, and when it is partial (spec 3.1 §7.27, §13 Q11).
            Some(config) => {
                let (covers, does_not_cover) =
                    crate::snapshot::safety_covers(config, inputs.plan_rewind);
                SafetyEntry::new("rewind", &covers, &does_not_cover)
            }
            None => SafetyEntry::new(
                "rewind",
                if inputs.plan_rewind {
                    "the plan graph's position only"
                } else {
                    "nothing: no rewind in this build"
                },
                "files: no file is restored; nor pushes, payments, sent messages or webhooks",
            ),
        });
        let mut help_topics: Vec<String> =
            ["commands", "cli <subcommand>", "manifest"].into_iter().map(String::from).collect();
        help_topics.sort();
        Self {
            harness_version: env!("CARGO_PKG_VERSION").to_string(),
            surface: inputs.surface,
            default_mode: inputs.default_mode,
            classes,
            members: inputs.members,
            packs,
            web: inputs.web,
            memory: inputs.memory,
            safety,
            languages: Vec::new(),
            multi_file_edit: inputs
                .rows
                .iter()
                .any(|(tool, _)| tool == crate::edits::APPLY_PATCH)
                .then(|| crate::edits::preferred_multi_file_edit(inputs.model).to_string()),
            background_shells: inputs
                .rows
                .iter()
                .any(|(tool, _)| tool == crate::shell::SHELL_KILL)
                .then(|| background_note(inputs.shell)),
            help_topics,
        }
    }

    /// The `<harness>` block of the system instruction (spec 3.1 §7.25): sorted, with no
    /// timestamp and no session data, so it is byte-identical for every build with the
    /// same configuration; at most [`HARNESS_BLOCK_MAX_BYTES`], ending with the sections
    /// it cut and `harness_help <topic>` when longer.
    pub fn render(&self) -> String {
        let header = format!(
            "<harness version=\"{}\" surface=\"{} {}\">\n",
            self.harness_version, self.surface.name, self.surface.version
        );
        let footer = "</harness>";
        let mut sections: Vec<(&str, String)> = vec![("classes", self.render_classes())];
        sections.push(("commands", self.render_commands(false)));
        sections.push(("setup", self.render_setup()));
        sections.push(("safety", self.render_safety()));
        sections.push(("flags", self.render_commands(true)));
        sections.push((
            "help",
            format!(
                "harness_help explains the harness on demand; topics: {}, or a page name.\n",
                self.help_topics.join(", ")
            ),
        ));
        let reserve = 160;
        let mut out = header;
        let mut cut = Vec::new();
        for (name, text) in sections {
            if text.is_empty() {
                continue;
            }
            // A section that does not fit is cut; the smaller ones after it still go in.
            if out.len() + text.len() + footer.len() + reserve <= HARNESS_BLOCK_MAX_BYTES {
                out.push_str(&text);
            } else {
                cut.push(name);
            }
        }
        if !cut.is_empty() {
            let _ = writeln!(
                out,
                "Cut for length: {}. harness_help <topic> returns them.",
                cut.join(", ")
            );
        }
        out.push_str(footer);
        out
    }

    fn render_classes(&self) -> String {
        let mut out = String::from("Classes (read-only | plan | ask | auto):\n");
        for class in &self.classes {
            let decisions =
                class.decisions.iter().map(|d| d.label()).collect::<Vec<_>>().join(" | ");
            let mut what = class.tools.join(" ");
            if let Some(note) = &class.note {
                if !what.is_empty() {
                    what.push_str("; ");
                }
                what.push_str(note);
            }
            if what.is_empty() {
                let _ = writeln!(out, "  {}: {decisions}", class.class);
            } else {
                let _ = writeln!(out, "  {} ({what}): {decisions}", class.class);
            }
        }
        out.push_str(
            "Standing rules can turn \"ask\" into allow or deny; the reminder lists the ones that \
             apply. \"ask every time\" is never covered by a rule.\n",
        );
        out
    }

    /// The surface's commands with their summaries: the slash commands and subcommands, or
    /// with `flags` the flags.
    fn render_commands(&self, flags: bool) -> String {
        let mut commands: Vec<CommandEntry> = self
            .surface
            .commands
            .iter()
            .filter(|command| (command.kind == CommandKind::Flag) == flags)
            .cloned()
            .collect();
        if commands.is_empty() {
            return String::new();
        }
        commands.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
        let mut out = if flags {
            String::from("Flags the person passes:\n")
        } else {
            String::from(
                "Commands the person types (you cannot run them; when input looks like a \
                 mistyped command, name the closest one and say what it does, and never claim a \
                 command ran):\n",
            )
        };
        for command in &commands {
            let _ = writeln!(out, "  {}  {}", command.typed(&self.surface.name), command.summary);
        }
        out
    }

    fn render_setup(&self) -> String {
        let mut out = String::new();
        if let Some(kernel) = self.surface.kernel {
            let _ = writeln!(
                out,
                "Kernel: {}",
                match (kernel.compiled, kernel.running) {
                    (_, true) => "this session runs under the agent kernel",
                    (true, false) => "compiled in; this session does not run under it",
                    (false, false) => "not compiled into this build",
                }
            );
        }
        if !self.members.is_empty() {
            let members: Vec<String> = self
                .members
                .iter()
                .map(|m| format!("{} ({}, {} tools)", m.name, m.kind, m.tools))
                .collect();
            let _ = writeln!(out, "Members: {}", members.join(", "));
        }
        let packs: Vec<String> = self
            .packs
            .iter()
            .map(|pack| match (pack.state, &pack.enable) {
                (PackState::Active, _) => format!("{} (active)", pack.name),
                (PackState::Off, Some(how)) => format!("{} (off: {how})", pack.name),
                (PackState::Off, None) => format!("{} (off)", pack.name),
                (PackState::NotCompiled, _) => format!("{} (not compiled)", pack.name),
            })
            .collect();
        let _ = writeln!(out, "Packs: {}", packs.join(", "));
        let web = match &self.web {
            WebSearchPath::Provider { provider, .. } => format!("{provider}'s built-in search"),
            WebSearchPath::Governed { provider } => {
                format!("the governed web_search over {provider}")
            }
            WebSearchPath::None { reason } => format!("none ({reason})"),
        };
        let _ = writeln!(out, "Web search: {web}");
        let memory = match (self.memory.working_notes, self.memory.long_term_facts) {
            (false, false) => "off".to_string(),
            (notes, facts) => {
                let mut parts = Vec::new();
                if notes {
                    parts.push("working notes".to_string());
                }
                if facts {
                    parts.push(format!(
                        "long-term facts ({})",
                        self.memory.embedder.as_deref().unwrap_or("lexical recall")
                    ));
                }
                parts.join(", ")
            }
        };
        let _ = writeln!(out, "Memory: {memory}");
        if let Some(tool) = &self.multi_file_edit {
            let _ = writeln!(
                out,
                "Edits: edit_file takes several replacements in one file as edits; apply_patch \
                 changes several files, all or none; prefer {tool} across files. An edit of a \
                 file changed since you read it is refused as stale: read it again."
            );
        }
        out.push_str(&self.render_background());
        out
    }

    fn render_background(&self) -> String {
        self.background_shells
            .as_ref()
            .map(|note| format!("Background shells: {note}\n"))
            .unwrap_or_default()
    }

    fn render_safety(&self) -> String {
        let mut out = String::from("Safety (promise only what these cover):\n");
        for entry in &self.safety {
            let _ = writeln!(
                out,
                "  {}: covers {}; does not cover {}",
                entry.mechanism, entry.covers, entry.does_not_cover
            );
        }
        out
    }

    /// The answer of `harness_help` for `topic` (spec 3.1 §7.25), or `None` for a topic it
    /// does not know. Pages come from `pages`, by name.
    pub(crate) fn help(
        &self,
        topic: Option<&str>,
        pages: &BTreeMap<String, String>,
    ) -> Option<String> {
        let topic = topic.map(str::trim).filter(|topic| !topic.is_empty());
        let body = match topic {
            None => {
                let mut topics = self.help_topics.clone();
                topics.extend(pages.keys().cloned());
                format!("Topics: {}\n\n{}", topics.join(", "), self.render())
            }
            Some("commands") => {
                let commands =
                    format!("{}{}", self.render_commands(false), self.render_commands(true));
                if commands.is_empty() {
                    "This surface has no commands: the harness runs as a library.".to_string()
                } else {
                    commands
                }
            }
            Some("manifest") => serde_json::to_string_pretty(self).ok()?,
            Some(topic) if topic.starts_with("cli") => {
                let wanted = topic.trim_start_matches("cli").trim();
                let entry = self.surface.commands.iter().find(|command| {
                    command.kind == CommandKind::Subcommand && command.name == wanted
                })?;
                match &entry.usage {
                    Some(usage) => format!("{} {}\n\n{usage}", self.surface.name, entry.name),
                    None => format!("{} {}: {}", self.surface.name, entry.name, entry.summary),
                }
            }
            Some(page) => pages.get(page)?.clone(),
        };
        let topic = topic.unwrap_or("");
        let mut text = format!(
            "<harness-help topic=\"{topic}\" version=\"{}\">\n{body}\n</harness-help>",
            self.harness_version
        );
        if text.len() > HARNESS_HELP_MAX_BYTES {
            let mut end = HARNESS_HELP_MAX_BYTES - 64;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n[truncated]\n</harness-help>");
        }
        Some(text)
    }
}

/// What background shells suit under `shell` (spec 3.1 §7.29 "Isolation").
fn background_note(shell: Option<&crate::shell::IsolationStatus>) -> String {
    let base = "bash run_in_background returns a shell_id; a reminder names each that ends, \
                shell_output reads it, shell_kill stops it";
    match shell {
        Some(crate::shell::IsolationStatus::OsSandbox { .. }) => format!(
            "{base}. Under the sandbox they suit builds, tests and watchers; a server started \
             there is unreachable from later commands, so a server needs the host"
        ),
        _ => format!("{base}. Servers and watchers run on the host"),
    }
}

fn sandbox_entry(shell: &crate::shell::IsolationStatus) -> SafetyEntry {
    use crate::shell::IsolationStatus;
    match shell {
        IsolationStatus::OsSandbox { enforcer } => SafetyEntry {
            mechanism: format!("OS sandbox ({enforcer})"),
            covers: "bash writes outside the workspace and its scratch directory, and the network"
                .into(),
            does_not_cover: "the file tools, which run on the host under the policy".into(),
        },
        IsolationStatus::Container { image } => SafetyEntry {
            mechanism: format!("container ({image})"),
            covers: "bash runs in the container with the workspace mounted".into(),
            does_not_cover: "the workspace itself, which bash can change".into(),
        },
        IsolationStatus::Degraded { requested, .. } => SafetyEntry {
            mechanism: "sandbox".into(),
            covers: format!(
                "nothing: the {requested} sandbox is unavailable, so bash runs on the host"
            ),
            does_not_cover: "anything the user's account can reach".into(),
        },
        IsolationStatus::Host => SafetyEntry::new(
            "sandbox",
            "nothing: bash runs on the host with the user's permissions",
            "anything the user's account can reach",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(commands: usize) -> HarnessManifest {
        manifest_with(commands, None)
    }

    fn manifest_with(
        commands: usize,
        snapshots: Option<crate::snapshot::SnapshotConfig>,
    ) -> HarnessManifest {
        let surface =
            SurfaceManifest::new("adk-rust", "3.0.0")
                .commands((0..commands).map(|n| {
                    CommandEntry::flag(format!("--flag-{n:04}"), "a flag with a summary")
                }));
        HarnessManifest::generate(ManifestInputs {
            surface,
            default_mode: HarnessMode::Ask,
            rows: vec![
                ("read_file".into(), TableRow::ReadTodoDelegate),
                ("edit_file".into(), TableRow::Edit),
                ("bash".into(), TableRow::ShellHost),
            ],
            switches: PolicySwitches::default(),
            web_allowlist: &[],
            shell: None,
            unsandboxed_retry: false,
            members: Vec::new(),
            active_packs: &[],
            web: WebSearchPath::None { reason: "off".into() },
            memory: MemoryEntry::default(),
            ledger_durable: true,
            plan_rewind: false,
            snapshots,
            model: "scripted",
        })
    }

    /// The block says what a rewind restores only when snapshots are on, and when a
    /// snapshot is partial (spec 3.1 §7.27, §13 Q11).
    #[test]
    fn the_rewind_entry_follows_the_snapshot_configuration() {
        let off = manifest(0).render();
        assert!(off.contains("rewind: covers nothing: no rewind in this build"), "{off}");
        assert!(off.contains("no file is restored"), "{off}");
        let on = manifest_with(0, Some(crate::snapshot::SnapshotConfig::default())).render();
        assert!(on.contains("shell writes included"), "{on}");
        assert!(on.contains("/rewind restores those files"), "{on}");
        assert!(on.contains("reaches 50000 files is partial"), "{on}");
        assert!(!on.contains("no file is restored"), "{on}");
    }

    #[test]
    fn the_block_names_the_preferred_multi_file_edit_tool_for_the_model() {
        let generate = |model: &str| {
            HarnessManifest::generate(ManifestInputs {
                surface: SurfaceManifest::library(),
                default_mode: HarnessMode::Ask,
                rows: vec![
                    ("edit_file".into(), TableRow::Edit),
                    ("apply_patch".into(), TableRow::Edit),
                ],
                switches: PolicySwitches::default(),
                web_allowlist: &[],
                shell: None,
                unsandboxed_retry: false,
                members: Vec::new(),
                active_packs: &[],
                web: WebSearchPath::None { reason: "off".into() },
                memory: MemoryEntry::default(),
                ledger_durable: true,
                plan_rewind: false,
                snapshots: None,
                model,
            })
        };
        let openai = generate("gpt-5.6-terra");
        assert_eq!(openai.multi_file_edit.as_deref(), Some("apply_patch"));
        assert!(openai.render().contains("prefer apply_patch across files"));
        let anthropic = generate("claude-sonnet-5-5").render();
        assert!(anthropic.contains("prefer edit_file across files"), "{anthropic}");
        assert!(anthropic.contains("edit (apply_patch edit_file): deny | deny | ask | allow"));
        assert_eq!(manifest(3).multi_file_edit, None, "no apply_patch, no recommendation");
    }

    #[test]
    fn a_long_block_is_cut_at_its_cap_and_names_what_it_cut() {
        let block = manifest(400).render();
        assert!(block.len() <= HARNESS_BLOCK_MAX_BYTES, "{} bytes", block.len());
        assert!(block.contains("Cut for length: flags"), "{block}");
        assert!(block.contains("Safety"), "the sections after a cut one still go in: {block}");
        assert!(
            block.contains("edit (edit_file): deny | deny | ask | allow"),
            "classes are never cut"
        );
        assert!(block.ends_with("</harness>"));
    }

    #[test]
    fn a_short_block_cuts_nothing() {
        let block = manifest(3).render();
        assert!(!block.contains("Cut for length"), "{block}");
        assert!(block.contains("adk-rust --flag-0002  a flag with a summary"));
    }

    #[test]
    fn help_is_capped_and_unknown_topics_are_none() {
        let manifest = manifest(4000);
        let pages = BTreeMap::new();
        let commands = manifest.help(Some("commands"), &pages).unwrap();
        assert!(commands.len() <= HARNESS_HELP_MAX_BYTES, "{} bytes", commands.len());
        assert!(commands.ends_with("[truncated]\n</harness-help>"));
        assert!(manifest.help(Some("cli nope"), &pages).is_none());
        assert!(manifest.help(Some("nope"), &pages).is_none());
    }
}
