//! Agents a person adds, as every surface shows and changes them (spec 3.3 §5.7).
//!
//! The kernel keeps each agent in its store and changes it only through its audited agent
//! table (`adk_kernel::agents`); this module is the wire between that table and the surfaces:
//!
//! | Type | What a surface does with it |
//! |---|---|
//! | [`AgentDraft`] | The definition as **Set it up yourself** and **Settings** edit it |
//! | [`AgentDetail`] | One agent: its definition, version, state and how it was added |
//! | [`AgentPreview`] | **Here is what Finance can do**: the abilities, the risk and the digest a change is confirmed with; "Was … Now …" for a change; what removing it ends |
//! | [`EndedWork`] | What removing an agent stops, counted |
//! | [`AgentChoices`] | The models, packs, areas and colours a person may choose |
//! | [`AgentCardView`] | The definition list on the decision that adds a suggested agent |
//! | [`AgentPreviewRequest`] | What `POST /surface/agents/preview` previews |
//!
//! Every name and instruction is data: renderers escape it (S-I9). No surface shows an
//! agent's id (§8).

use adk_core::ApprovalRequest;
use adk_kernel::PartitionMode;
use adk_kernel::agents::{
    AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentUses, AvatarColor,
    FileAccess, ModelChoice, ShellAccess, WebAccess,
};
use chrono::{DateTime, FixedOffset, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{AgentRef, Avatar, Mode, ModeView};
use crate::project::CALL_TEXT_LIMIT;
use crate::rules::Risk;

/// The eight avatar colours, in the kernel's palette order (spec 3.3 §9).
const PALETTE: [&str; 8] =
    ["#C03A00", "#15803D", "#1D4ED8", "#7C3AED", "#475569", "#B45309", "#0E7490", "#BE185D"];

/// An agent's colour: one of the eight avatar colours, never a free value (spec 3.3 §5.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentColor {
    /// `#C03A00`.
    #[default]
    Orange,
    /// `#15803D`.
    Green,
    /// `#1D4ED8`.
    Blue,
    /// `#7C3AED`.
    Violet,
    /// `#475569`.
    Slate,
    /// `#B45309`.
    Amber,
    /// `#0E7490`.
    Teal,
    /// `#BE185D`.
    Pink,
}

impl AgentColor {
    /// The colour drawn, `#RRGGBB`.
    ///
    /// # Example
    ///
    /// ```rust
    /// assert_eq!(adk_surface::AgentColor::Green.hex(), "#15803D");
    /// ```
    pub fn hex(self) -> &'static str {
        PALETTE[AvatarColor::from(self).index()]
    }

    /// The colour's name on screen: "Green".
    pub fn label(self) -> &'static str {
        match self {
            Self::Orange => "Orange",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Violet => "Violet",
            Self::Slate => "Slate",
            Self::Amber => "Amber",
            Self::Teal => "Teal",
            Self::Pink => "Pink",
        }
    }
}

impl From<AgentColor> for AvatarColor {
    fn from(color: AgentColor) -> Self {
        match color {
            AgentColor::Orange => Self::Orange,
            AgentColor::Green => Self::Green,
            AgentColor::Blue => Self::Blue,
            AgentColor::Violet => Self::Violet,
            AgentColor::Slate => Self::Slate,
            AgentColor::Amber => Self::Amber,
            AgentColor::Teal => Self::Teal,
            AgentColor::Pink => Self::Pink,
        }
    }
}

impl From<AvatarColor> for AgentColor {
    fn from(color: AvatarColor) -> Self {
        match color {
            AvatarColor::Green => Self::Green,
            AvatarColor::Blue => Self::Blue,
            AvatarColor::Violet => Self::Violet,
            AvatarColor::Slate => Self::Slate,
            AvatarColor::Amber => Self::Amber,
            AvatarColor::Teal => Self::Teal,
            AvatarColor::Pink => Self::Pink,
            _ => Self::Orange,
        }
    }
}

/// Where an agent's files are: none, read, or read and change (spec 3.3 §5.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FileUse {
    /// No files.
    #[default]
    None,
    /// It reads files.
    Read,
    /// It reads and changes files.
    ReadWrite,
}

/// Where an agent's commands run: nowhere, in the sandbox, or on this computer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ShellUse {
    /// No shell.
    #[default]
    None,
    /// The sandboxed shell.
    Sandboxed,
    /// A shell on this computer.
    Host,
}

/// What of the web an agent reaches.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "access", rename_all = "camelCase")]
pub enum WebUse {
    /// No web.
    #[default]
    None,
    /// Listed sites only.
    #[serde(rename_all = "camelCase")]
    Listed {
        /// The sites, such as `example.com`.
        hosts: Vec<String>,
    },
    /// Any site.
    Any,
}

/// What an agent can use (spec 3.3 §5.7 "What it can use").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UsesDraft {
    /// Its files.
    #[serde(default)]
    pub files: FileUse,
    /// One folder inside its area's workspace, relative to it; none for the whole workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Its shell.
    #[serde(default)]
    pub shell: ShellUse,
    /// Its web.
    #[serde(default)]
    pub web: WebUse,
    /// Its packs.
    #[serde(default)]
    pub packs: Vec<String>,
    /// The drivers mounted for its area that it may use.
    #[serde(default)]
    pub drivers: Vec<String>,
}

/// A model an agent runs on; never a key (spec 3.3 decision 21).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    /// The provider: `anthropic`, `openai`, `gemini`.
    pub provider: String,
    /// The model, shown in mono: `claude-sonnet-5-5`.
    pub model: String,
}

/// An agent's definition as **Set it up yourself** and **Settings** edit it, in the order of
/// spec 3.3 §5.7 "The definition".
///
/// # Example
///
/// ```rust
/// use adk_surface::{AgentDraft, Mode};
///
/// let draft = AgentDraft::new("Finance", "default").with_mode(Mode::AskFirst);
/// let definition = draft.clone().into_definition();
/// assert_eq!(definition.display_name, "Finance");
/// assert_eq!(AgentDraft::from_definition(&definition), draft);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDraft {
    /// Its name: "Finance".
    pub display_name: String,
    /// Its colour.
    #[serde(default)]
    pub color: AgentColor,
    /// What it does, in one line.
    #[serde(default)]
    pub description: String,
    /// How it works.
    #[serde(default)]
    pub instructions: String,
    /// Its area.
    pub area: String,
    /// How much it asks first.
    pub mode: Mode,
    /// What it runs on; none for the primary agent's model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelView>,
    /// What it can use.
    #[serde(default)]
    pub uses: UsesDraft,
    /// Its daily spending limit, micro-USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit_micro_usd: Option<u64>,
}

fn mode_of(mode: PartitionMode) -> Mode {
    match mode {
        PartitionMode::ReadOnly => Mode::JustLook,
        PartitionMode::Plan => Mode::Suggest,
        PartitionMode::Auto => Mode::GoAhead,
        _ => Mode::AskFirst,
    }
}

fn partition_mode(mode: Mode) -> PartitionMode {
    match mode {
        Mode::JustLook => PartitionMode::ReadOnly,
        Mode::Suggest => PartitionMode::Plan,
        Mode::AskFirst => PartitionMode::Ask,
        Mode::GoAhead => PartitionMode::Auto,
    }
}

impl AgentDraft {
    /// A draft named `display_name` in `area`, asking first, using nothing, in the colour the
    /// name gives it.
    pub fn new(display_name: impl Into<String>, area: impl Into<String>) -> Self {
        let display_name = display_name.into();
        Self {
            color: AvatarColor::for_name(&display_name).into(),
            display_name,
            description: String::new(),
            instructions: String::new(),
            area: area.into(),
            mode: Mode::AskFirst,
            model: None,
            uses: UsesDraft::default(),
            daily_limit_micro_usd: None,
        }
    }

    /// Sets how much it asks first.
    #[must_use]
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets what it can use.
    #[must_use]
    pub fn with_uses(mut self, uses: UsesDraft) -> Self {
        self.uses = uses;
        self
    }

    /// The kernel's definition of this draft.
    pub fn into_definition(self) -> AgentDefinition {
        let mut uses = AgentUses::default()
            .with_files(match self.uses.files {
                FileUse::None => FileAccess::None,
                FileUse::Read => FileAccess::Read,
                FileUse::ReadWrite => FileAccess::ReadWrite,
            })
            .with_shell(match self.uses.shell {
                ShellUse::None => ShellAccess::None,
                ShellUse::Sandboxed => ShellAccess::Sandboxed,
                ShellUse::Host => ShellAccess::Host,
            })
            .with_web(match self.uses.web {
                WebUse::None => WebAccess::None,
                WebUse::Listed { hosts } => WebAccess::Listed { hosts },
                WebUse::Any => WebAccess::Any,
            });
        if let Some(folder) = self.uses.folder.filter(|folder| !folder.trim().is_empty()) {
            uses = uses.with_folder(folder);
        }
        for pack in self.uses.packs {
            uses = uses.with_pack(pack);
        }
        for driver in self.uses.drivers {
            uses = uses.with_driver(driver);
        }
        let mut definition = AgentDefinition::new(self.display_name, self.area.as_str())
            .with_color(self.color.into())
            .with_description(self.description)
            .with_instructions(self.instructions)
            .with_mode(partition_mode(self.mode))
            .with_uses(uses);
        if let Some(model) = self.model {
            definition = definition.with_model(ModelChoice::new(model.provider, model.model));
        }
        if let Some(limit) = self.daily_limit_micro_usd {
            definition = definition.with_daily_limit(limit);
        }
        definition
    }

    /// The draft of a kernel definition.
    pub fn from_definition(definition: &AgentDefinition) -> Self {
        let uses = &definition.uses;
        Self {
            display_name: definition.display_name.clone(),
            color: definition.color.into(),
            description: definition.description.clone(),
            instructions: definition.instructions.clone(),
            area: definition.area.as_str().to_string(),
            mode: mode_of(definition.mode),
            model: definition.model.as_ref().map(|model| ModelView {
                provider: model.provider.clone(),
                model: model.model.clone(),
            }),
            uses: UsesDraft {
                files: match uses.files {
                    FileAccess::None => FileUse::None,
                    FileAccess::Read => FileUse::Read,
                    FileAccess::ReadWrite => FileUse::ReadWrite,
                },
                folder: uses.folder.clone(),
                shell: match uses.shell {
                    ShellAccess::None => ShellUse::None,
                    ShellAccess::Sandboxed => ShellUse::Sandboxed,
                    ShellAccess::Host => ShellUse::Host,
                },
                web: match &uses.web {
                    WebAccess::None => WebUse::None,
                    WebAccess::Listed { hosts } => WebUse::Listed { hosts: hosts.clone() },
                    WebAccess::Any => WebUse::Any,
                },
                packs: uses.packs.clone(),
                drivers: uses.drivers.clone(),
            },
            daily_limit_micro_usd: definition.daily_limit_micro_usd,
        }
    }
}

/// Where an agent is in its life, as a surface shows it (spec 3.3 §5.7, §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentLife {
    /// Suggested by an agent and waiting for a person.
    Suggested,
    /// Added: **Ready** until it runs.
    Active,
    /// **Paused** by hand.
    Paused,
    /// Being removed.
    Removing,
    /// Removed; its threads stay.
    Removed,
    /// A suggestion the kernel refused.
    Refused,
}

impl From<AgentRecordState> for AgentLife {
    fn from(state: AgentRecordState) -> Self {
        match state {
            AgentRecordState::Proposed => Self::Suggested,
            AgentRecordState::Paused => Self::Paused,
            AgentRecordState::Removing => Self::Removing,
            AgentRecordState::Removed => Self::Removed,
            AgentRecordState::Refused => Self::Refused,
            _ => Self::Active,
        }
    }
}

/// One agent a person added (spec 3.3 §5.7): what **Settings** shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentDetail {
    /// Its kernel target, for commands. Never shown.
    pub id: String,
    /// The agent as threads and decisions name it.
    pub agent: AgentRef,
    /// Its definition.
    pub draft: AgentDraft,
    /// The version a change is made against. Never shown.
    pub version: u64,
    /// Where it is in its life.
    pub state: AgentLife,
    /// "Added by you on 25 Oct", or "Suggested by ADK-Rust, added by you on 25 Oct".
    pub origin_text: String,
    /// How much it asks first, with its words.
    pub mode: ModeView,
    /// Why the kernel cannot build it: "No key for Anthropic".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cant_start: Option<String>,
    /// When it was last changed.
    pub changed_at: DateTime<Utc>,
}

/// What removing an agent ends, counted (spec 3.3 §5.7 "Pausing and removing").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EndedWork {
    /// Pieces of work stopped.
    pub work: u32,
    /// Routines ended.
    pub routines: u32,
    /// Decisions waiting for you that end without an answer.
    pub decisions: u32,
    /// Follow-ups withdrawn.
    pub follow_ups: u32,
    /// "Allowed without asking" rules removed.
    pub rules: u32,
    /// "Removing Finance stops 1 piece of work, ends 2 decisions waiting for you and 1
    /// routine. Its threads stay."
    pub text: String,
}

impl EndedWork {
    /// The counts of the kernel's `ended`, worded for `name`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_surface::EndedWork;
    ///
    /// let mut ended = adk_kernel::agents::EndedWork::default();
    /// ended.work = vec!["p1".into()];
    /// ended.decisions = vec!["d1".into(), "d2".into()];
    /// ended.routines = vec!["r1".into()];
    /// assert_eq!(
    ///     EndedWork::of(&ended, "Finance").text,
    ///     "Removing Finance stops 1 piece of work, ends 2 decisions waiting for you and 1 routine. Its threads stay."
    /// );
    /// ```
    pub fn of(ended: &adk_kernel::agents::EndedWork, name: &str) -> Self {
        let count = |items: &[String]| u32::try_from(items.len()).unwrap_or(u32::MAX);
        let mut out = Self {
            work: count(&ended.work),
            routines: count(&ended.routines),
            decisions: count(&ended.decisions),
            follow_ups: count(&ended.follow_ups),
            rules: u32::try_from(ended.rules).unwrap_or(u32::MAX),
            text: String::new(),
        };
        out.text = words::removing_ends(name, &out);
        out
    }
}

/// **Here is what Finance can do** (spec 3.3 §5.7): what a change lets an agent do, its risk,
/// and the digest the change is confirmed with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPreview {
    /// The agent's kernel target; for an agent not added yet, the id it would get. Never shown.
    pub id: String,
    /// The change: `create`, `change`, `pause`, `resume` or `remove`.
    pub action: String,
    /// "Here is what Finance can do".
    pub heading: String,
    /// What it can do, one plain sentence each.
    pub abilities: Vec<String>,
    /// The risk of the result.
    pub risk: Risk,
    /// "Medium risk".
    pub risk_text: String,
    /// It can spend money.
    pub money: bool,
    /// The change lets the agent do more: it needs what adding it at that risk needs, and it
    /// first ends the agent's waiting decisions.
    pub widens: bool,
    /// Confirming needs presence: "Confirm it's you" in the home, or a paired local terminal.
    pub needs_presence: bool,
    /// The fields a change changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// "Was": the definition before a change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<AgentDraft>,
    /// "Now": the definition after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<AgentDraft>,
    /// What removing it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<EndedWork>,
    /// The digest the change applies with. Never shown.
    pub digest: String,
    /// The first six characters of the digest: the terminal's `--confirm` code.
    pub code: String,
}

impl AgentPreview {
    /// The kernel's preview of a change to the agent named `name`, in the surfaces' words.
    pub fn of(preview: &adk_kernel::agents::AgentPreview, name: &str) -> Self {
        let risk = Risk::from(preview.risk);
        Self {
            id: preview.id.clone(),
            action: preview.action.clone(),
            heading: words::here_is_what_can_do(name),
            abilities: preview.abilities.clone(),
            risk,
            risk_text: risk.words().to_string(),
            money: preview.money,
            widens: preview.widens,
            needs_presence: preview.needs_presence(),
            fields: preview.fields.clone(),
            was: preview.was.as_ref().map(AgentDraft::from_definition),
            now: preview.now.as_ref().map(AgentDraft::from_definition),
            ended: preview.ended.as_ref().map(|ended| EndedWork::of(ended, name)),
            digest: preview.digest.clone(),
            code: preview.code().to_string(),
        }
    }
}

/// What `POST /surface/agents/preview` previews (spec 3.3 §5.7 "Show, then confirm").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum AgentPreviewRequest {
    /// Adding an agent: **Set it up yourself**, or **Change it first** on a suggestion.
    #[serde(rename_all = "camelCase")]
    Create {
        /// The definition.
        draft: AgentDraft,
        /// The suggestion it replaces, when it came from **Change it first**.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<String>,
    },
    /// Changing an agent at the version seen.
    #[serde(rename_all = "camelCase")]
    Change {
        /// The agent.
        agent: String,
        /// The version seen.
        version: u64,
        /// The definition after the change.
        draft: AgentDraft,
    },
    /// Removing an agent at the version seen.
    #[serde(rename_all = "camelCase")]
    Remove {
        /// The agent.
        agent: String,
        /// The version seen.
        version: u64,
    },
}

impl AgentPreviewRequest {
    /// The kernel's action of this request.
    pub fn into_action(self) -> adk_kernel::agents::AgentAction {
        match self {
            Self::Create { draft, replaces } => adk_kernel::agents::AgentAction::Create {
                definition: draft.into_definition(),
                replaces,
            },
            Self::Change { agent, version, draft } => adk_kernel::agents::AgentAction::Change {
                id: agent,
                expected_version: version,
                definition: draft.into_definition(),
            },
            Self::Remove { agent, version } => {
                adk_kernel::agents::AgentAction::Remove { id: agent, expected_version: version }
            }
        }
    }
}

/// An area a person may put an agent in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AreaChoice {
    /// The area's id.
    pub id: String,
    /// Its name: "Home".
    pub name: String,
    /// Its mode: an agent there asks at least this much.
    pub mode: ModeView,
    /// The drivers mounted for it.
    #[serde(default)]
    pub drivers: Vec<String>,
}

/// A colour a person may choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ColorChoice {
    /// The colour.
    pub color: AgentColor,
    /// Its name: "Green".
    pub label: String,
    /// What is drawn, `#RRGGBB`.
    pub hex: String,
}

/// What a person may choose for an agent on this kernel (spec 3.3 §5.7, decision 21): the
/// models of the providers whose key the kernel has, the packs, the areas and the colours.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentChoices {
    /// The models, the primary agent's first.
    pub models: Vec<ModelView>,
    /// The packs.
    pub packs: Vec<String>,
    /// The areas.
    pub areas: Vec<AreaChoice>,
    /// The eight colours.
    pub colors: Vec<ColorChoice>,
}

impl AgentChoices {
    /// The kernel's choices, with `areas`.
    pub fn of(choices: &adk_kernel::agents::AgentChoices, areas: Vec<AreaChoice>) -> Self {
        Self {
            models: choices
                .models
                .iter()
                .map(|model| ModelView {
                    provider: model.provider.clone(),
                    model: model.model.clone(),
                })
                .collect(),
            packs: choices.packs.clone(),
            areas,
            colors: AvatarColor::ALL
                .iter()
                .map(|color| {
                    let color = AgentColor::from(*color);
                    ColorChoice {
                        color,
                        label: color.label().to_string(),
                        hex: color.hex().to_string(),
                    }
                })
                .collect(),
        }
    }
}

/// One line of an agent's definition on a card: "Area · Home".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardLine {
    /// "Runs on".
    pub label: String,
    /// "claude-sonnet-5-5".
    pub value: String,
    /// The value is a technical name, shown in mono. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mono: bool,
}

/// The definition on the decision that adds a suggested agent (spec 3.3 §5.7 "The decision
/// that adds an agent", S-I13): every field, from the inbox entry the kernel wrote, never from
/// the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentCardView {
    /// The id the suggested agent would get: what **Change it first** replaces. Never shown.
    pub id: String,
    /// The agent it would add.
    pub agent: AgentRef,
    /// "What it does", in one line.
    pub description: String,
    /// The definition, one line per field: area, how much it asks first, what it runs on,
    /// files, shell, web, packs, drivers, the daily limit.
    pub lines: Vec<CardLine>,
    /// The instructions, quoted, cut short past the call limit.
    pub instructions: String,
    /// The instructions are cut short: they must be expanded before the agent can be added.
    pub instructions_truncated: bool,
    /// What it can do, in plain words.
    pub abilities: Vec<String>,
    /// "It can do less than ADK-Rust: no shell, no web".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compared: Option<String>,
    /// What it would start with: "Nothing yet".
    pub allowed_without_asking: String,
    /// The suggestion, for **Change it first**.
    pub draft: AgentDraft,
}

/// The avatar of an agent named `display_name` in `color`.
pub(crate) fn avatar_of(display_name: &str, color: AgentColor) -> Avatar {
    let monogram: String = display_name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    Avatar { monogram, color: color.hex().to_string() }
}

/// The agent a record names, with its own colour (spec 3.3 §5.7: kept across a rename).
pub(crate) fn agent_ref(record: &AgentRecord) -> AgentRef {
    AgentRef {
        name: record.id.clone(),
        display_name: record.display_name.clone(),
        avatar: avatar_of(&record.display_name, record.color.into()),
    }
}

/// The mode with its words.
pub(crate) fn mode_view(mode: PartitionMode) -> ModeView {
    crate::project::mode_view(adk_harness::HarnessMode::from(mode))
}

/// "25 Oct".
fn day(at: DateTime<Utc>, offset: FixedOffset) -> String {
    at.with_timezone(&offset).format("%-d %b").to_string()
}

impl AgentDetail {
    /// The detail of `record` for `user`, with the kernel's reason it cannot start, when it
    /// cannot; `primary` names the suggesting agent ("ADK-Rust").
    pub fn of(
        record: &AgentRecord,
        cant_start: Option<String>,
        user: &str,
        primary: &str,
        offset: FixedOffset,
    ) -> Self {
        let you = |subject: &adk_core::principal::SubjectRef| {
            if subject.id == user { "you".to_string() } else { subject.id.clone() }
        };
        let added_by = record.approved_by.as_ref().unwrap_or(&record.created_by);
        let on = day(record.created_at, offset);
        let origin_text = match &record.origin {
            AgentOrigin::ByHand => format!("Added by {} on {on}", you(&record.created_by)),
            AgentOrigin::Suggested { .. } => {
                format!(
                    "{}, added by {} on {on}",
                    words::suggested_agent_by(primary),
                    you(added_by)
                )
            }
        };
        Self {
            id: record.id.clone(),
            agent: agent_ref(record),
            draft: AgentDraft::from_definition(&record.definition()),
            version: record.version,
            state: record.state.into(),
            origin_text,
            mode: mode_view(record.mode),
            cant_start,
            changed_at: record.updated_at,
        }
    }
}

/// What a definition's files, shell and web are, in the card's words.
fn uses_lines(uses: &AgentUses) -> Vec<CardLine> {
    let line =
        |label: &str, value: String| CardLine { label: label.to_string(), value, mono: false };
    let place = uses.folder.clone().unwrap_or_else(|| "the workspace".to_string());
    let mut lines = vec![
        line(
            "Files",
            match uses.files {
                FileAccess::None => "No files".to_string(),
                FileAccess::Read => format!("Reads files in {place}"),
                FileAccess::ReadWrite => format!("Reads and changes files in {place}"),
            },
        ),
        line(
            "Shell",
            match uses.shell {
                ShellAccess::None => "No shell",
                ShellAccess::Sandboxed => "In the sandbox",
                ShellAccess::Host => "On this computer",
            }
            .to_string(),
        ),
        line(
            "Web",
            match &uses.web {
                WebAccess::None => "No web".to_string(),
                WebAccess::Listed { hosts } => format!("Listed sites only: {}", hosts.join(", ")),
                WebAccess::Any => "Any site".to_string(),
            },
        ),
    ];
    if !uses.packs.is_empty() {
        lines.push(line("Packs", uses.packs.join(", ")));
    }
    if !uses.drivers.is_empty() {
        lines.push(line("Drivers", uses.drivers.join(", ")));
    }
    lines
}

/// What an agent lacks that its suggester may have, in plain words.
fn lacks(uses: &AgentUses) -> Vec<&'static str> {
    let mut lacks = Vec::new();
    if uses.files == FileAccess::None {
        lacks.push("no files");
    }
    if uses.shell == ShellAccess::None {
        lacks.push("no shell");
    }
    if uses.web == WebAccess::None {
        lacks.push("no web");
    }
    lacks
}

impl AgentCardView {
    /// The card of the suggestion `request` (`kernel:agent.create`), whose arguments are the
    /// definition; `None` when they are not one. `suggester` is the suggesting agent's display
    /// name, `area` its area's name.
    pub fn of_request(request: &ApprovalRequest, suggester: &str, area: &str) -> Option<Self> {
        let definition: AgentDefinition =
            serde_json::from_value(request.args_redacted.clone()).ok()?;
        let record = AgentRecord::new(
            String::new(),
            definition.clone(),
            AgentRecordState::Proposed,
            AgentOrigin::ByHand,
            request
                .requester
                .clone()
                .unwrap_or_else(|| adk_core::principal::SubjectRef::human(&request.scope.user_id)),
            request.created_at,
        );
        let assessment = adk_harness::assess_agent(&record);
        let mut lines = vec![
            CardLine { label: "Area".into(), value: area.to_string(), mono: false },
            CardLine {
                label: "How much it asks first".into(),
                value: mode_view(definition.mode).status,
                mono: false,
            },
        ];
        lines.push(match &definition.model {
            Some(model) => {
                CardLine { label: "Runs on".into(), value: model.model.clone(), mono: true }
            }
            None => CardLine {
                label: "Runs on".into(),
                value: format!("{suggester}'s model"),
                mono: false,
            },
        });
        lines.extend(uses_lines(&definition.uses));
        if let Some(limit) = definition.daily_limit_micro_usd {
            lines.push(CardLine {
                label: "Daily spending limit".into(),
                value: crate::words::dollars(limit),
                mono: false,
            });
        }
        let truncated = definition.instructions.chars().count() > CALL_TEXT_LIMIT;
        let instructions = if truncated {
            definition.instructions.chars().take(CALL_TEXT_LIMIT).collect::<String>() + "…"
        } else {
            definition.instructions.clone()
        };
        let lacking = lacks(&definition.uses);
        let compared = Some(if lacking.is_empty() {
            format!("It can do no more than {suggester}.")
        } else {
            format!("It can do less than {suggester}: {}.", lacking.join(", "))
        });
        // The kernel names a suggestion's request `agent-<id>-<digest[..12]>`.
        let id = request
            .id
            .strip_prefix("agent-")
            .and_then(|rest| rest.rsplit_once('-'))
            .map(|(id, _)| id.to_string())
            .unwrap_or_default();
        Some(Self {
            agent: AgentRef {
                name: id.clone(),
                display_name: definition.display_name.clone(),
                avatar: avatar_of(&definition.display_name, definition.color.into()),
            },
            description: definition.description.clone(),
            lines,
            instructions,
            instructions_truncated: truncated,
            abilities: assessment.abilities,
            compared,
            allowed_without_asking: words::NOTHING_YET.to_string(),
            draft: AgentDraft::from_definition(&definition),
            id,
        })
    }
}

/// An area's name on screen: "Home" for the kernel's default area, else its id capitalised.
pub(crate) fn area_name(area: &str) -> String {
    if area == adk_kernel::PartitionId::DEFAULT {
        return "Home".to_string();
    }
    let mut chars = area.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The kernel's caller for a surface's `caller` (spec 3.3 §5.7, S-I18): who, from which
/// surface and how, the paired device, and whether the person is present.
pub(crate) fn agent_caller(caller: &crate::origin::Caller) -> adk_kernel::agents::AgentCaller {
    let surface = serde_json::to_value(caller.surface)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut agent =
        adk_kernel::agents::AgentCaller::new(caller.subject.clone(), surface, caller.origin.kind());
    if let Some(device) = &caller.device {
        agent = agent.with_device(device.clone());
    }
    if caller.presence_confirmed {
        agent = agent.with_presence(adk_kernel::agents::Presence::Confirmed);
    } else if caller.paired_local {
        agent = agent.with_presence(adk_kernel::agents::Presence::PairedLocal);
    }
    agent
}

/// The agents a person added and the changes to them, over a kernel's agent table (spec 3.3
/// §5.7): what `LocalSource` reads and does.
pub(crate) mod local {
    use adk_kernel::agents::{AgentAction, AgentRecordState, Agents};
    use chrono::FixedOffset;

    use super::{
        AgentChoices, AgentDetail, AgentPreview, AgentPreviewRequest, AreaChoice, EndedWork,
        agent_caller, area_name, mode_view,
    };
    use crate::error::{Result, SurfaceError};
    use crate::origin::Caller;

    /// The agents of `user` the kernel keeps: active, paused and suggested ones.
    pub(crate) async fn definitions(
        agents: &Agents,
        user: &str,
        primary: &str,
        offset: FixedOffset,
    ) -> Result<Vec<AgentDetail>> {
        Ok(agents
            .list()
            .await?
            .into_iter()
            .map(|view| AgentDetail::of(&view.agent, view.cant_start, user, primary, offset))
            .collect())
    }

    /// One agent of `user`.
    pub(crate) async fn agent(
        agents: &Agents,
        id: &str,
        user: &str,
        primary: &str,
        offset: FixedOffset,
    ) -> Result<AgentDetail> {
        agents
            .get(id)
            .await?
            .filter(|view| {
                !matches!(view.agent.state, AgentRecordState::Removed | AgentRecordState::Refused)
            })
            .map(|view| AgentDetail::of(&view.agent, view.cant_start, user, primary, offset))
            .ok_or_else(|| SurfaceError::not_found(format!("agent '{id}'")))
    }

    /// What a person may choose, with the kernel's areas.
    pub(crate) async fn choices(
        agents: &Agents,
        partitions: &adk_kernel::Partitions,
    ) -> Result<AgentChoices> {
        let choices = agents.choices().await?;
        let mut areas: Vec<AreaChoice> = partitions
            .store()
            .list()
            .await?
            .into_iter()
            .map(|area| AreaChoice {
                name: area_name(area.id.as_str()),
                id: area.id.as_str().to_string(),
                mode: mode_view(area.default_mode),
                drivers: area.drivers.clone(),
            })
            .collect();
        if areas.is_empty() {
            areas.push(AreaChoice {
                id: adk_kernel::PartitionId::DEFAULT.to_string(),
                name: area_name(adk_kernel::PartitionId::DEFAULT),
                mode: mode_view(adk_kernel::PartitionMode::Ask),
                drivers: Vec::new(),
            });
        }
        Ok(AgentChoices::of(&choices, areas))
    }

    /// The preview of `request` for `caller`.
    pub(crate) async fn preview(
        agents: &Agents,
        caller: &Caller,
        request: AgentPreviewRequest,
    ) -> Result<AgentPreview> {
        let action = request.into_action();
        let name = name_of(agents, &action).await;
        let preview = agents.preview(&agent_caller(caller), &action).await?;
        Ok(AgentPreview::of(&preview, &name))
    }

    /// The name the change is about: the definition's, or the agent's now.
    async fn name_of(agents: &Agents, action: &AgentAction) -> String {
        match action {
            AgentAction::Create { definition, .. } | AgentAction::Change { definition, .. } => {
                definition.display_name.clone()
            }
            other => match other.id() {
                Some(id) => agents
                    .get(id)
                    .await
                    .ok()
                    .flatten()
                    .map(|view| view.agent.display_name)
                    .unwrap_or_else(|| id.to_string()),
                None => String::new(),
            },
        }
    }

    /// What a change applied, in the surfaces' words.
    pub(crate) enum Applied {
        Agent(Box<AgentDetail>),
        Removed(EndedWork),
    }

    /// Applies `action` for `caller` with the digest its preview showed.
    pub(crate) async fn apply(
        agents: &Agents,
        caller: &Caller,
        action: AgentAction,
        digest: &str,
        primary: &str,
        offset: FixedOffset,
    ) -> Result<Applied> {
        let name = name_of(agents, &action).await;
        let outcome = agents.apply(&agent_caller(caller), action, digest).await?;
        let user = caller.user_id();
        Ok(match outcome {
            adk_kernel::agents::AgentOutcome::Removed { ended, .. } => {
                Applied::Removed(EndedWork::of(&ended, &name))
            }
            adk_kernel::agents::AgentOutcome::Created { agent }
            | adk_kernel::agents::AgentOutcome::Changed { agent }
            | adk_kernel::agents::AgentOutcome::Paused { agent }
            | adk_kernel::agents::AgentOutcome::Resumed { agent } => {
                Applied::Agent(Box::new(AgentDetail::of(&agent, None, user, primary, offset)))
            }
            other => {
                return Err(SurfaceError::Invalid(format!("the kernel answered {other:?}")));
            }
        })
    }

    /// Whether `error` says the agent changed since the version the surface showed.
    pub(crate) fn out_of_date(error: &SurfaceError) -> bool {
        error.code() == adk_kernel::error::AGENT_OUT_OF_DATE_CODE
    }
}

/// The roster with the person's own agents (spec 3.3 §5.7 "The roster"): each process row of
/// an added agent takes its name, colour, area and mode from its definition; an agent that has
/// not run is listed as **Ready**, a paused one as **Paused**, one the kernel cannot build as
/// **Can't start**. The primary agent comes first, then the person's agents by name.
pub(crate) fn roster(
    rows: Vec<crate::model::AgentRow>,
    definitions: &[AgentDetail],
    can_change: bool,
) -> Vec<crate::model::AgentRow> {
    use crate::model::{AgentRow, AgentStatus};
    let (mut primary, mut added): (Vec<AgentRow>, Vec<AgentRow>) = rows
        .into_iter()
        .partition(|row| !definitions.iter().any(|detail| detail.id == row.agent.name));
    for detail in definitions {
        if !matches!(detail.state, AgentLife::Active | AgentLife::Paused) {
            continue;
        }
        let status = if detail.cant_start.is_some() {
            Some((AgentStatus::CantStart, words::CANT_START))
        } else if detail.state == AgentLife::Paused {
            Some((AgentStatus::Paused, words::PAUSED))
        } else {
            None
        };
        let area = Some(area_name(&detail.draft.area));
        match added.iter_mut().find(|row| row.agent.name == detail.id) {
            Some(row) => {
                row.agent = detail.agent.clone();
                if let Some((status, text)) = status {
                    row.status = status;
                    row.status_text = text.to_string();
                    row.wake_at = None;
                }
                row.area = area;
                row.mode = Some(detail.mode.clone());
                row.added = true;
                row.can_change = can_change;
            }
            None => {
                let (status, text) = status.unwrap_or((AgentStatus::Ready, words::READY));
                added.push(AgentRow {
                    agent: detail.agent.clone(),
                    status,
                    status_text: text.to_string(),
                    wake_at: None,
                    session_id: None,
                    area,
                    mode: Some(detail.mode.clone()),
                    added: true,
                    can_change,
                });
            }
        }
    }
    // Added agents follow the primary, by name.
    added.sort_by(|a, b| {
        (!a.added, a.agent.display_name.to_lowercase())
            .cmp(&(!b.added, b.agent.display_name.to_lowercase()))
    });
    primary.extend(added.into_iter().filter(|row| row.added));
    primary
}

/// The first steps of "How it got here" for a suggested agent (spec 3.3 §5.7): when the person
/// asked, what the suggesting agent read in that turn — file names and sites only — and that it
/// suggested the agent. `events` is the suggesting session's history.
pub(crate) fn suggestion_steps(
    events: &[adk_core::Event],
    suggester: &str,
    suggested_at: DateTime<Utc>,
    hh_mm: impl Fn(DateTime<Utc>) -> String,
) -> Vec<String> {
    let before: Vec<&adk_core::Event> =
        events.iter().filter(|event| event.timestamp <= suggested_at).collect();
    let asked = before.iter().rposition(|event| event.author == "user");
    let mut steps = Vec::new();
    if let Some(at) = asked {
        steps.push(format!("You asked at {}", hh_mm(before[at].timestamp)));
    }
    let turn = &before[asked.map_or(0, |at| at + 1)..];
    let mut files: Vec<String> = Vec::new();
    let mut sites: Vec<String> = Vec::new();
    for event in turn {
        let Some(content) = &event.llm_response.content else { continue };
        for part in &content.parts {
            let adk_core::Part::FunctionCall { name, args, .. } = part else { continue };
            match name.as_str() {
                "read_file" | "grep" | "glob" | "list_dir" => {
                    if let Some(path) = args.get("path").and_then(serde_json::Value::as_str) {
                        let file = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
                        if !files.contains(&file) {
                            files.push(file);
                        }
                    }
                }
                "web_fetch" => {
                    let host = args
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|url| url.split("://").nth(1))
                        .and_then(|rest| rest.split(['/', '?', '#', ':']).next())
                        .map(str::to_string);
                    if let Some(host) = host.filter(|host| !sites.contains(host)) {
                        sites.push(host);
                    }
                }
                _ => {}
            }
        }
    }
    match files.len() {
        0 => {}
        1 => steps.push(format!("{suggester} read {}", files[0])),
        n => steps.push(format!("{suggester} read {n} files")),
    }
    if !sites.is_empty() {
        steps.push(format!("{suggester} read {}", sites.join(", ")));
    }
    steps.push(format!("{suggester} suggested this agent"));
    steps
}

/// The words of agents a person adds live in [`words`](crate::words) (spec 3.3 §8).
pub(crate) use crate::words;

#[cfg(test)]
mod tests {
    use adk_core::principal::SubjectRef;

    use super::words::*;
    use super::*;

    #[test]
    fn a_draft_round_trips_through_the_kernels_definition() {
        let draft = AgentDraft {
            display_name: "Finance".into(),
            color: AgentColor::Teal,
            description: "Reads invoices".into(),
            instructions: "Draft reminders.".into(),
            area: "home".into(),
            mode: Mode::Suggest,
            model: Some(ModelView {
                provider: "anthropic".into(),
                model: "claude-sonnet-5-5".into(),
            }),
            uses: UsesDraft {
                files: FileUse::Read,
                folder: Some("Invoices".into()),
                shell: ShellUse::Sandboxed,
                web: WebUse::Listed { hosts: vec!["bank.example".into()] },
                packs: vec!["browser".into()],
                drivers: vec!["calendar".into()],
            },
            daily_limit_micro_usd: Some(2_000_000),
        };
        let definition = draft.clone().into_definition();
        assert!(definition.validate().is_ok());
        assert_eq!(AgentDraft::from_definition(&definition), draft);
        let wire = serde_json::to_value(&draft).unwrap();
        assert_eq!(
            wire["uses"]["web"],
            serde_json::json!({ "access": "listed", "hosts": ["bank.example"] })
        );
        assert_eq!(wire["color"], "teal");
    }

    #[test]
    fn the_roster_puts_the_primary_first_and_the_persons_agents_by_name() {
        use crate::model::{AgentRow, AgentStatus};
        let detail = |id: &str, name: &str, state: AgentRecordState| {
            let record = AgentRecord::new(
                id,
                AgentDefinition::new(name, "default"),
                state,
                AgentOrigin::ByHand,
                SubjectRef::human("ana"),
                chrono::Utc::now(),
            );
            AgentDetail::of(&record, None, "ana", "ADK-Rust", FixedOffset::east_opt(0).unwrap())
        };
        let row = |name: &str| AgentRow {
            agent: AgentRef {
                name: name.into(),
                display_name: name.into(),
                avatar: avatar_of(name, AgentColor::Slate),
            },
            status: AgentStatus::Working,
            status_text: "Working".into(),
            wake_at: None,
            session_id: Some(format!("s-{name}")),
            area: None,
            mode: None,
            added: false,
            can_change: false,
        };
        let definitions = [
            detail("zeta", "Zeta", AgentRecordState::Active),
            detail("bills", "Bills", AgentRecordState::Paused),
            detail("waiting", "Waiting", AgentRecordState::Proposed),
        ];
        let rows = roster(vec![row("zeta"), row("adk-rust")], &definitions, true);
        let names: Vec<(&str, AgentStatus)> =
            rows.iter().map(|row| (row.agent.display_name.as_str(), row.status)).collect();
        assert_eq!(
            names,
            [
                ("adk-rust", AgentStatus::Working),
                ("Bills", AgentStatus::Paused),
                ("Zeta", AgentStatus::Working)
            ]
        );
        assert!(rows[1].added && rows[1].can_change && !rows[0].added);
        assert_eq!(rows[2].session_id.as_deref(), Some("s-zeta"));
    }

    #[test]
    fn colours_follow_the_palette() {
        for (index, color) in AvatarColor::ALL.iter().enumerate() {
            assert_eq!(AgentColor::from(*color).hex(), PALETTE[index]);
            assert_eq!(AvatarColor::from(AgentColor::from(*color)), *color);
        }
    }

    #[test]
    fn removing_names_what_ends() {
        let ended = |work, decisions, routines| EndedWork {
            work,
            decisions,
            routines,
            ..EndedWork::default()
        };
        assert_eq!(
            removing_ends("Finance", &ended(1, 2, 1)),
            "Removing Finance stops 1 piece of work, ends 2 decisions waiting for you and 1 routine. Its threads stay."
        );
        assert_eq!(
            removing_ends("Bills", &ended(0, 1, 1)),
            "Removing Bills ends 1 decision waiting for you and 1 routine. Its threads stay."
        );
        assert_eq!(
            removing_ends("Bills", &ended(0, 0, 0)),
            "Removing Bills ends nothing that is running. Its threads stay."
        );
    }

    #[test]
    fn the_detail_says_how_the_agent_was_added() {
        let record = AgentRecord::new(
            "finance",
            AgentDefinition::new("Finance", "default"),
            AgentRecordState::Active,
            AgentOrigin::Suggested {
                agent: adk_core::AgentId::new("urn:adk:agent:adk-rust/adk-rust").unwrap(),
                session: "s1".into(),
                run: None,
                unattended: false,
            },
            SubjectRef::agent(&adk_core::AgentId::new("urn:adk:agent:adk-rust/adk-rust").unwrap()),
            "2026-10-25T09:12:00Z".parse().unwrap(),
        )
        .with_approved_by(SubjectRef::human("ana"));
        let detail =
            AgentDetail::of(&record, None, "ana", "ADK-Rust", FixedOffset::east_opt(0).unwrap());
        assert_eq!(detail.origin_text, "Suggested by ADK-Rust, added by you on 25 Oct");
        assert_eq!(detail.state, AgentLife::Active);
        assert_eq!(detail.agent.avatar.color, AgentColor::from(record.color).hex());
    }

    #[test]
    fn the_card_shows_the_definition_in_full_from_the_inbox_entry() {
        let definition = AgentDefinition::new("Finance", "default")
            .with_mode(PartitionMode::Ask)
            .with_instructions("Read invoices.\nDraft reminders.")
            .with_uses(AgentUses::default().with_files(FileAccess::Read).with_folder("Invoices"));
        let call = adk_core::ToolConfirmationRequest::new(
            "kernel:agent.create",
            serde_json::to_value(&definition).unwrap(),
        );
        let request = adk_core::ApprovalRequest::new(
            "agent-finance-0123",
            adk_core::ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            adk_core::ApprovalPolicy::new([adk_core::ApproverSelector::human("ana")]),
        );
        let card = AgentCardView::of_request(&request, "ADK-Rust", "Home").unwrap();
        let line =
            |label: &str| card.lines.iter().find(|line| line.label == label).unwrap().value.clone();
        assert_eq!(line("Area"), "Home");
        assert_eq!(line("How much it asks first"), "Asks first");
        assert_eq!(line("Files"), "Reads files in Invoices");
        assert_eq!(line("Shell"), "No shell");
        assert_eq!(
            card.compared.as_deref(),
            Some("It can do less than ADK-Rust: no shell, no web.")
        );
        assert_eq!(card.allowed_without_asking, NOTHING_YET);
        assert!(!card.instructions_truncated);
    }
}
