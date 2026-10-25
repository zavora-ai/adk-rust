//! An agent a person adds: its definition and the record the kernel keeps (spec 3.3 §5.7).

use std::fmt;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, AgentId, AgentPrincipal, ErrorCategory, ErrorComponent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AGENT_INVALID_CODE, KernelError, Result};
use crate::partition::{Partition, PartitionId, PartitionMode};

/// The longest id the kernel gives an agent.
pub const MAX_AGENT_ID_LEN: usize = 40;
/// The longest name an agent may have, in characters (spec 3.3 §5.7).
pub const MAX_AGENT_NAME_LEN: usize = 40;
/// The longest "What it does" line, in characters.
pub const MAX_AGENT_DESCRIPTION_LEN: usize = 200;
/// The longest instructions an agent may have, in characters.
pub const MAX_AGENT_INSTRUCTIONS_LEN: usize = 16_000;
/// The id an agent gets when its name has no letter or digit (spec 3.3 §5.7).
pub const FALLBACK_AGENT_ID: &str = "agent";
/// Ids no added agent ever gets, beside any containing `@` or `.` and the host's own
/// targets (spec 3.3 §5.7).
pub const RESERVED_AGENT_IDS: [&str; 2] = ["adk-rust", "self-improver"];
/// The name no added agent may take beside the primary agent's (spec 3.3 §5.7).
pub const RESERVED_AGENT_NAME: &str = "You";

fn invalid(reason: impl Into<String>) -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        AGENT_INVALID_CODE,
        reason.into(),
    ))
}

/// One of the eight avatar colours the surfaces draw an agent's monogram in (spec 3.3 §5.7,
/// §9), in the order of `adk-surface`'s avatar palette. The kernel keeps the name, never a
/// colour value.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::AvatarColor;
///
/// assert_eq!(AvatarColor::ALL.len(), 8);
/// assert_eq!(serde_json::to_value(AvatarColor::Teal).unwrap(), "teal");
/// assert_eq!(AvatarColor::for_name("Finance"), AvatarColor::for_name("Finance"));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AvatarColor {
    /// The first colour of the palette.
    #[default]
    Orange,
    /// The second.
    Green,
    /// The third.
    Blue,
    /// The fourth.
    Violet,
    /// The fifth.
    Slate,
    /// The sixth.
    Amber,
    /// The seventh.
    Teal,
    /// The eighth.
    Pink,
}

impl AvatarColor {
    /// Every colour, in palette order.
    pub const ALL: [Self; 8] = [
        Self::Orange,
        Self::Green,
        Self::Blue,
        Self::Violet,
        Self::Slate,
        Self::Amber,
        Self::Teal,
        Self::Pink,
    ];

    /// The colour's position in the palette, 0 to 7.
    pub fn index(self) -> usize {
        match self {
            Self::Orange => 0,
            Self::Green => 1,
            Self::Blue => 2,
            Self::Violet => 3,
            Self::Slate => 4,
            Self::Amber => 5,
            Self::Teal => 6,
            Self::Pink => 7,
        }
    }

    /// The colour the surfaces draw `display_name` in when nothing chose one: the FNV-1a hash
    /// of its bytes over the palette, as `adk-surface` picks an avatar colour.
    pub fn for_name(display_name: &str) -> Self {
        let hash = display_name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        Self::ALL[(hash % Self::ALL.len() as u64) as usize]
    }

    /// The colour's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Orange => "orange",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Violet => "violet",
            Self::Slate => "slate",
            Self::Amber => "amber",
            Self::Teal => "teal",
            Self::Pink => "pink",
        }
    }
}

impl fmt::Display for AvatarColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl std::str::FromStr for AvatarColor {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self> {
        let text = text.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|color| color.as_str() == text).ok_or_else(|| {
            invalid(format!(
                "unknown colour `{text}`; use one of {}",
                Self::ALL.map(Self::as_str).join(", ")
            ))
        })
    }
}

/// The provider and model an agent runs on (spec 3.3 §5.7, decision 21). Never a key: keys
/// stay in the kernel's environment.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::ModelChoice;
///
/// let model = ModelChoice::new("anthropic", "claude-sonnet-5-5");
/// assert_eq!(serde_json::to_value(&model).unwrap()["provider"], "anthropic");
/// assert!(model.validate().is_ok());
/// assert!(ModelChoice::new("anthropic", "sk-ant key").validate().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct ModelChoice {
    /// The provider: `anthropic`, `openai`, `gemini`, …
    pub provider: String,
    /// The model id, such as `claude-sonnet-5-5`.
    pub model: String,
}

impl ModelChoice {
    /// The model `model` of `provider`.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self { provider: provider.into(), model: model.into() }
    }

    /// Checks both are short identifiers: non-empty, at most 100 characters, without
    /// whitespace or control characters.
    ///
    /// # Errors
    ///
    /// Returns a `kernel.agent.invalid` error naming the field.
    pub fn validate(&self) -> Result<()> {
        for (field, value) in [("provider", &self.provider), ("model", &self.model)] {
            if value.is_empty()
                || value.chars().count() > 100
                || value.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(invalid(format!(
                    "the model's {field} must be a name of at most 100 characters without \
                     spaces, such as `claude-sonnet-5-5`; keys are never part of an agent"
                )));
            }
        }
        Ok(())
    }
}

/// What an agent may do with files in its area's workspace (spec 3.3 §5.7).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::FileAccess;
///
/// assert!(FileAccess::Read < FileAccess::ReadWrite);
/// assert_eq!(serde_json::to_value(FileAccess::ReadWrite).unwrap(), "readWrite");
/// ```
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum FileAccess {
    /// No files.
    #[default]
    None,
    /// Read files.
    Read,
    /// Read and change files.
    ReadWrite,
}

/// Whether an agent has a shell (spec 3.3 §5.7).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::ShellAccess;
///
/// assert!(ShellAccess::Sandboxed < ShellAccess::Host);
/// assert_eq!(serde_json::to_value(ShellAccess::Host).unwrap(), "host");
/// ```
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum ShellAccess {
    /// No shell.
    #[default]
    None,
    /// A shell in the sandbox.
    Sandboxed,
    /// A shell on this computer.
    Host,
}

/// Which sites an agent may reach (spec 3.3 §5.7).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::WebAccess;
///
/// let listed = WebAccess::listed(["Example.com", "docs.rs", "example.com"]);
/// assert_eq!(listed, WebAccess::Listed { hosts: vec!["docs.rs".into(), "example.com".into()] });
/// assert_eq!(serde_json::to_value(&listed).unwrap()["access"], "listed");
/// assert!(WebAccess::None.within(&listed) && listed.within(&WebAccess::Any));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "access", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WebAccess {
    /// No site.
    #[default]
    None,
    /// Only these hosts.
    Listed {
        /// The hosts, lowercase, sorted and unique.
        hosts: Vec<String>,
    },
    /// Any site.
    Any,
}

impl WebAccess {
    /// The hosts `hosts`, lowercased, sorted and unique.
    pub fn listed(hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut hosts: Vec<String> =
            hosts.into_iter().map(|host| host.into().trim().to_ascii_lowercase()).collect();
        hosts.sort();
        hosts.dedup();
        Self::Listed { hosts }
    }

    /// How much it opens: 0 for none, 1 for listed hosts, 2 for any site.
    pub fn reach(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::Listed { .. } => 1,
            Self::Any => 2,
        }
    }

    /// Whether every site `self` reaches, `ceiling` reaches too.
    pub fn within(&self, ceiling: &Self) -> bool {
        match (self, ceiling) {
            (Self::None, _) | (_, Self::Any) => true,
            (Self::Listed { hosts }, Self::Listed { hosts: allowed }) => {
                hosts.iter().all(|host| allowed.contains(host))
            }
            (Self::Listed { .. } | Self::Any, Self::None | Self::Listed { .. }) => false,
        }
    }

    fn normalized(self) -> Self {
        match self {
            Self::Listed { hosts } => Self::listed(hosts),
            other => other,
        }
    }
}

/// What an agent can use (spec 3.3 §5.7): files in its area's workspace or one folder
/// inside it, a shell, the web, packs, and drivers mounted for its area. Adding an agent
/// never mounts a driver (K-I13).
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentUses, FileAccess, WebAccess};
///
/// let uses = AgentUses::default()
///     .with_files(FileAccess::Read)
///     .with_folder("Invoices")
///     .with_web(WebAccess::listed(["example.com"]));
/// let wire = serde_json::to_value(&uses).unwrap();
/// assert_eq!(wire["files"], "read");
/// assert_eq!(wire["folder"], "Invoices");
/// assert_eq!(wire["shell"], "none");
/// assert_eq!(serde_json::from_value::<AgentUses>(wire).unwrap(), uses);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct AgentUses {
    /// Files in the area's workspace, or in [`folder`](Self::folder).
    #[serde(default)]
    pub files: FileAccess,
    /// One folder inside the area's workspace, relative to it; `None` for the whole
    /// workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// A shell.
    #[serde(default)]
    pub shell: ShellAccess,
    /// The web.
    #[serde(default)]
    pub web: WebAccess,
    /// The packs it has, by name, sorted and unique.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<String>,
    /// The drivers it uses, by id, each mounted for its area, sorted and unique.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<String>,
}

impl AgentUses {
    /// Sets file access.
    #[must_use]
    pub fn with_files(mut self, files: FileAccess) -> Self {
        self.files = files;
        self
    }

    /// Narrows files to `folder`, relative to the area's workspace.
    #[must_use]
    pub fn with_folder(mut self, folder: impl Into<String>) -> Self {
        self.folder = Some(folder.into());
        self
    }

    /// Sets the shell.
    #[must_use]
    pub fn with_shell(mut self, shell: ShellAccess) -> Self {
        self.shell = shell;
        self
    }

    /// Sets the web.
    #[must_use]
    pub fn with_web(mut self, web: WebAccess) -> Self {
        self.web = web;
        self
    }

    /// Adds a pack.
    #[must_use]
    pub fn with_pack(mut self, pack: impl Into<String>) -> Self {
        self.packs.push(pack.into());
        self
    }

    /// Adds a driver.
    #[must_use]
    pub fn with_driver(mut self, driver: impl Into<String>) -> Self {
        self.drivers.push(driver.into());
        self
    }

    /// The same uses with packs, drivers and hosts sorted and unique, and the folder in its
    /// canonical form (`/` separators, no `.` parts, no trailing `/`).
    #[must_use]
    pub fn normalized(mut self) -> Self {
        for list in [&mut self.packs, &mut self.drivers] {
            for item in list.iter_mut() {
                *item = item.trim().to_string();
            }
            list.sort();
            list.dedup();
        }
        self.web = self.web.normalized();
        self.folder = self.folder.and_then(|folder| {
            let parts: Vec<&str> =
                folder.split(['/', '\\']).filter(|part| !part.is_empty() && *part != ".").collect();
            (!parts.is_empty()).then(|| parts.join("/"))
        });
        self
    }

    /// Whether `self` allows nothing `ceiling` does not, dimension by dimension; the names of
    /// the dimensions over it otherwise, in plain words.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::agents::{AgentUses, FileAccess, ShellAccess};
    ///
    /// let suggester = AgentUses::default().with_files(FileAccess::ReadWrite).with_folder("Docs");
    /// let ok = AgentUses::default().with_files(FileAccess::Read).with_folder("Docs/Invoices");
    /// assert!(ok.over(&suggester).is_empty());
    /// let over = AgentUses::default()
    ///     .with_files(FileAccess::Read)
    ///     .with_folder("Mail")
    ///     .with_shell(ShellAccess::Sandboxed);
    /// assert_eq!(over.over(&suggester), ["the folder Mail", "a shell in the sandbox"]);
    /// ```
    pub fn over(&self, ceiling: &Self) -> Vec<String> {
        let mine = self.clone().normalized();
        let ceiling = ceiling.clone().normalized();
        let mut over = Vec::new();
        if mine.files > ceiling.files {
            over.push(
                match mine.files {
                    FileAccess::ReadWrite => "changing files",
                    _ => "reading files",
                }
                .to_string(),
            );
        }
        if mine.files != FileAccess::None
            && let Some(allowed) = &ceiling.folder
        {
            let inside = mine.folder.as_ref().is_some_and(|folder| {
                folder == allowed || folder.starts_with(&format!("{allowed}/"))
            });
            if !inside {
                over.push(match &mine.folder {
                    Some(folder) => format!("the folder {folder}"),
                    None => "files outside the folder it may use".to_string(),
                });
            }
        }
        if mine.shell > ceiling.shell {
            over.push(
                match mine.shell {
                    ShellAccess::Host => "a shell on this computer",
                    _ => "a shell in the sandbox",
                }
                .to_string(),
            );
        }
        if !mine.web.within(&ceiling.web) {
            over.push(match &mine.web {
                WebAccess::Any => "any site".to_string(),
                WebAccess::Listed { hosts } => {
                    let extra: Vec<&str> = hosts
                        .iter()
                        .filter(|host| match &ceiling.web {
                            WebAccess::Listed { hosts: allowed } => !allowed.contains(host),
                            _ => true,
                        })
                        .map(String::as_str)
                        .collect();
                    format!("the sites {}", extra.join(", "))
                }
                WebAccess::None => String::new(),
            });
        }
        for pack in mine.packs.iter().filter(|pack| !ceiling.packs.contains(pack)) {
            over.push(format!("the {pack} pack"));
        }
        for driver in mine.drivers.iter().filter(|driver| !ceiling.drivers.contains(driver)) {
            over.push(format!("the {driver} driver"));
        }
        over
    }

    fn validate(&self) -> Result<()> {
        if let Some(folder) = &self.folder {
            let absolute = folder.starts_with('/')
                || folder.starts_with('\\')
                || folder.starts_with('~')
                || folder.as_bytes().get(1) == Some(&b':')
                || std::path::Path::new(folder).is_absolute();
            if absolute
                || folder.split(['/', '\\']).any(|part| part == "..")
                || folder.chars().any(char::is_control)
            {
                return Err(invalid(format!(
                    "the folder `{folder}` must be a folder inside the area's workspace, \
                     written relative to it without `..`, such as `Invoices`"
                )));
            }
        }
        if let WebAccess::Listed { hosts } = &self.web {
            if hosts.is_empty() {
                return Err(invalid(
                    "listed sites need at least one host; use no web access instead",
                ));
            }
            for host in hosts {
                let valid = !host.is_empty()
                    && host.len() <= 253
                    && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
                    && !host.starts_with(['.', '-'])
                    && !host.ends_with(['.', '-']);
                if !valid {
                    return Err(invalid(format!(
                        "`{host}` is not a host name; list sites as `example.com`, without a \
                         scheme or a path"
                    )));
                }
            }
        }
        for (what, list) in [("pack", &self.packs), ("driver", &self.drivers)] {
            if let Some(bad) = list.iter().find(|item| {
                item.trim().is_empty() || item.len() > 100 || item.chars().any(char::is_control)
            }) {
                return Err(invalid(format!("`{bad}` is not a {what} name")));
            }
        }
        Ok(())
    }
}

/// Where an agent is in its life (spec 3.3 §5.7). Never named `AgentState` or
/// `AgentStatus`, which `adk-surface` uses for a roster row.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::AgentRecordState;
///
/// assert_eq!(serde_json::to_value(AgentRecordState::Proposed).unwrap(), "proposed");
/// assert!(AgentRecordState::Paused.holds_its_name());
/// assert!(!AgentRecordState::Removed.holds_its_name());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentRecordState {
    /// Another agent suggested it; it waits for a person in the inbox.
    Proposed,
    /// Added; the kernel hosts it.
    Active,
    /// Paused by a person: its processes are suspended and it is not given new work.
    Paused,
    /// Being removed; the next tick on any replica finishes the removal.
    Removing,
    /// Removed: a tombstone, so its id is never given again (S-I14).
    Removed,
    /// A suggestion that ended refused, such as over its suggester's limits at approval.
    Refused,
}

impl AgentRecordState {
    /// The state's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Removing => "removing",
            Self::Removed => "removed",
            Self::Refused => "refused",
        }
    }

    /// Whether an agent in this state keeps its name from every other agent: active, paused
    /// and suggested agents do (spec 3.3 §5.7).
    pub fn holds_its_name(self) -> bool {
        matches!(self, Self::Proposed | Self::Active | Self::Paused)
    }

    /// Whether the kernel hosts an agent in this state: active and paused agents, so a
    /// paused agent's parked work stays resumable.
    pub fn is_hosted(self) -> bool {
        matches!(self, Self::Active | Self::Paused)
    }
}

impl fmt::Display for AgentRecordState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// How an agent was added (spec 3.3 §5.7).
///
/// # Example
///
/// ```rust
/// use adk_core::AgentId;
/// use adk_kernel::agents::AgentOrigin;
///
/// let origin = AgentOrigin::Suggested {
///     agent: AgentId::new("urn:adk:agent:adk-rust/adk-rust").unwrap(),
///     session: "s-1".into(),
///     run: None,
///     unattended: false,
/// };
/// let wire = serde_json::to_value(&origin).unwrap();
/// assert_eq!(wire["kind"], "suggested");
/// assert_eq!(serde_json::to_value(AgentOrigin::ByHand).unwrap()["kind"], "byHand");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentOrigin {
    /// A person set it up in the web home or the terminal.
    ByHand,
    /// An agent suggested it and a person approved the suggestion.
    Suggested {
        /// The suggesting agent.
        agent: AgentId,
        /// The session it was suggested in.
        session: String,
        /// The durable run that suggested it, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        /// Whether it was suggested during work the person did not start at a screen: a
        /// process that is not interactive, or a turn that came from a chat message. Such a
        /// suggestion is high risk (decision 20).
        #[serde(default)]
        unattended: bool,
    },
}

/// What a person defines when they add an agent (spec 3.3 §5.7): every field a decision
/// shows and the digest binds, and nothing the kernel keeps about its life.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentDefinition, AgentUses, FileAccess};
/// use adk_kernel::PartitionMode;
///
/// let finance = AgentDefinition::new("Finance", "default")
///     .with_description("Reads invoices and drafts payment reminders")
///     .with_instructions("Draft reminders; never send them.")
///     .with_mode(PartitionMode::Ask)
///     .with_uses(AgentUses::default().with_files(FileAccess::Read).with_folder("Invoices"));
/// assert!(finance.validate().is_ok());
/// assert_eq!(finance.digest(), finance.clone().digest());
/// assert_ne!(finance.digest(), finance.clone().with_mode(PartitionMode::Plan).digest());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct AgentDefinition {
    /// The name on screen, such as "Finance".
    pub display_name: String,
    /// The avatar colour, kept across a rename.
    pub color: AvatarColor,
    /// What it does, one line.
    #[serde(default)]
    pub description: String,
    /// How it works. Instructions shape what it says and tries, never what it may do.
    #[serde(default)]
    pub instructions: String,
    /// Its area; fixed once it is added.
    pub area: PartitionId,
    /// How much it asks first: its area's mode or stricter.
    pub mode: PartitionMode,
    /// The model it runs on; `None` for the primary agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelChoice>,
    /// What it can use.
    #[serde(default)]
    pub uses: AgentUses,
    /// Its daily spending limit in micro-USD; at most its area's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit_micro_usd: Option<u64>,
}

impl AgentDefinition {
    /// An agent named `display_name` in `area`, in its name's colour, in `ask` mode, using
    /// nothing.
    pub fn new(display_name: impl Into<String>, area: impl Into<PartitionId>) -> Self {
        let display_name = display_name.into();
        Self {
            color: AvatarColor::for_name(display_name.trim()),
            display_name,
            description: String::new(),
            instructions: String::new(),
            area: area.into(),
            mode: PartitionMode::Ask,
            model: None,
            uses: AgentUses::default(),
            daily_limit_micro_usd: None,
        }
    }

    /// Sets the colour.
    #[must_use]
    pub fn with_color(mut self, color: AvatarColor) -> Self {
        self.color = color;
        self
    }

    /// Sets what it does.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the instructions.
    #[must_use]
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    /// Sets how much it asks first.
    #[must_use]
    pub fn with_mode(mut self, mode: PartitionMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the model.
    #[must_use]
    pub fn with_model(mut self, model: ModelChoice) -> Self {
        self.model = Some(model);
        self
    }

    /// Sets what it can use.
    #[must_use]
    pub fn with_uses(mut self, uses: AgentUses) -> Self {
        self.uses = uses;
        self
    }

    /// Sets the daily spending limit.
    #[must_use]
    pub fn with_daily_limit(mut self, micro_usd: u64) -> Self {
        self.daily_limit_micro_usd = Some(micro_usd);
        self
    }

    /// The definition in canonical form: the name and line trimmed, the uses
    /// [normalized](AgentUses::normalized). Two definitions that differ only in form have
    /// one digest.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.display_name = self.display_name.trim().to_string();
        self.description = self.description.trim().to_string();
        self.uses = self.uses.normalized();
        self
    }

    /// Checks the rules every definition keeps on its own (spec 3.3 §5.7): a name of 1 to
    /// [`MAX_AGENT_NAME_LEN`] characters that is not "You"; one line of what it does; at
    /// most [`MAX_AGENT_INSTRUCTIONS_LEN`] characters of instructions; an area; a model
    /// name, not a key; a folder inside the workspace, without `..`; host names; a limit
    /// above zero. The rules that need the kernel — names taken, the area's limits — are the
    /// agent table's.
    ///
    /// # Errors
    ///
    /// Returns a `kernel.agent.invalid` error saying which rule the definition breaks.
    pub fn validate(&self) -> Result<()> {
        let name = self.display_name.trim();
        let length = name.chars().count();
        if length == 0 || length > MAX_AGENT_NAME_LEN || name.chars().any(char::is_control) {
            return Err(invalid(format!(
                "an agent's name is 1 to {MAX_AGENT_NAME_LEN} characters on one line"
            )));
        }
        if name.eq_ignore_ascii_case(RESERVED_AGENT_NAME) {
            return Err(invalid(format!("\"{name}\" is reserved; choose another name")));
        }
        let description = self.description.trim();
        if description.chars().count() > MAX_AGENT_DESCRIPTION_LEN
            || description.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "\"What it does\" is one line of at most {MAX_AGENT_DESCRIPTION_LEN} characters"
            )));
        }
        if self.instructions.chars().count() > MAX_AGENT_INSTRUCTIONS_LEN {
            return Err(invalid(format!(
                "instructions are at most {MAX_AGENT_INSTRUCTIONS_LEN} characters; shorten them"
            )));
        }
        if self.instructions.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r')) {
            return Err(invalid("instructions may not hold control characters"));
        }
        if self.area.as_str().trim().is_empty() || self.area.as_str().chars().any(char::is_control)
        {
            return Err(invalid("an agent needs an area"));
        }
        if let Some(model) = &self.model {
            model.validate()?;
        }
        if self.daily_limit_micro_usd == Some(0) {
            return Err(invalid(
                "a daily spending limit is above zero; leave it out for the area's limit",
            ));
        }
        self.uses.validate()
    }

    /// The digest a decision binds to: the SHA-256, in lowercase hex, of the canonical JSON
    /// of the [normalized](Self::normalized) definition (spec 3.3 §5.7). It covers every
    /// definition field and nothing else: not the state, version, timestamps or origin.
    pub fn digest(&self) -> String {
        use sha2::Digest as _;

        let canonical = serde_json::to_vec(&self.clone().normalized()).unwrap_or_default();
        sha2::Sha256::digest(&canonical).iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// An agent the kernel keeps (spec 3.3 §5.7, §10 K7): its definition, beside its id, state,
/// how it was added, who added and approved it, when, a version per change and the digest of
/// its definition.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState};
///
/// let record = AgentRecord::new(
///     "finance",
///     AgentDefinition::new("Finance", "default"),
///     AgentRecordState::Active,
///     AgentOrigin::ByHand,
///     SubjectRef::human("alice"),
///     chrono::Utc::now(),
/// );
/// assert_eq!(record.version, 1);
/// assert_eq!(record.digest, record.definition().digest());
/// assert!(record.validate().is_ok());
/// let wire = serde_json::to_value(&record).unwrap();
/// assert_eq!(wire["displayName"], "Finance");
/// assert_eq!(serde_json::from_value::<AgentRecord>(wire).unwrap(), record);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct AgentRecord {
    /// The id, given once from its first name: its kernel target, its root agent name and
    /// the last part of its identity, `urn:adk:agent:{app}/{id}`.
    pub id: String,
    /// The name on screen.
    pub display_name: String,
    /// The avatar colour.
    pub color: AvatarColor,
    /// What it does, one line.
    #[serde(default)]
    pub description: String,
    /// How it works.
    #[serde(default)]
    pub instructions: String,
    /// Its area.
    pub area: PartitionId,
    /// How much it asks first.
    pub mode: PartitionMode,
    /// The model it runs on; `None` for the primary agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelChoice>,
    /// What it can use.
    #[serde(default)]
    pub uses: AgentUses,
    /// Its daily spending limit in micro-USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit_micro_usd: Option<u64>,
    /// Where it is in its life.
    pub state: AgentRecordState,
    /// How it was added.
    pub origin: AgentOrigin,
    /// Who added it: the person who set it up, or the suggesting agent.
    pub created_by: SubjectRef,
    /// The person who approved its suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<SubjectRef>,
    /// When it was first stored.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
    /// 1 when first stored, one more with every change; the compare-and-set token.
    pub version: u64,
    /// The digest of its definition ([`AgentDefinition::digest`]).
    pub digest: String,
}

impl AgentRecord {
    /// A record of `definition` under `id`, at version 1, created at `at`.
    pub fn new(
        id: impl Into<String>,
        definition: AgentDefinition,
        state: AgentRecordState,
        origin: AgentOrigin,
        created_by: SubjectRef,
        at: DateTime<Utc>,
    ) -> Self {
        let definition = definition.normalized();
        Self {
            id: id.into(),
            digest: definition.digest(),
            display_name: definition.display_name,
            color: definition.color,
            description: definition.description,
            instructions: definition.instructions,
            area: definition.area,
            mode: definition.mode,
            model: definition.model,
            uses: definition.uses,
            daily_limit_micro_usd: definition.daily_limit_micro_usd,
            state,
            origin,
            created_by,
            approved_by: None,
            created_at: at,
            updated_at: at,
            version: 1,
        }
    }

    /// Its definition.
    pub fn definition(&self) -> AgentDefinition {
        AgentDefinition {
            display_name: self.display_name.clone(),
            color: self.color,
            description: self.description.clone(),
            instructions: self.instructions.clone(),
            area: self.area.clone(),
            mode: self.mode,
            model: self.model.clone(),
            uses: self.uses.clone(),
            daily_limit_micro_usd: self.daily_limit_micro_usd,
        }
    }

    /// The record with `definition` in place of its own and the digest recomputed; the
    /// area is the record's own whatever `definition` says (K-I15). The version and times
    /// are the agent table's to move.
    #[must_use]
    pub fn with_definition(mut self, definition: AgentDefinition) -> Self {
        let definition = definition.normalized();
        self.digest = AgentDefinition { area: self.area.clone(), ..definition.clone() }.digest();
        self.display_name = definition.display_name;
        self.color = definition.color;
        self.description = definition.description;
        self.instructions = definition.instructions;
        self.mode = definition.mode;
        self.model = definition.model;
        self.uses = definition.uses;
        self.daily_limit_micro_usd = definition.daily_limit_micro_usd;
        self
    }

    /// Sets the state.
    #[must_use]
    pub fn with_state(mut self, state: AgentRecordState) -> Self {
        self.state = state;
        self
    }

    /// Sets the person who approved it.
    #[must_use]
    pub fn with_approved_by(mut self, by: SubjectRef) -> Self {
        self.approved_by = Some(by);
        self
    }

    /// Checks the id ([`check_agent_id`]) and the definition ([`AgentDefinition::validate`]),
    /// and that the digest is the definition's.
    ///
    /// # Errors
    ///
    /// Returns a `kernel.agent.invalid` error saying which rule the record breaks.
    pub fn validate(&self) -> Result<()> {
        check_agent_id(&self.id)?;
        self.definition().validate()?;
        if self.digest != self.definition().digest() {
            return Err(invalid(format!(
                "agent `{}`: its digest is not its definition's; the record was changed \
                 outside the agent table",
                self.id
            )));
        }
        Ok(())
    }

    /// The agent's identity in `area` (spec 3.3 §5.7): `urn:adk:agent:{app}/{id}` with the
    /// area's class, tenant and partition attribute, and as its sponsor the person who
    /// approved it, or who added it by hand.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::principal::SubjectRef;
    /// use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState};
    /// use adk_kernel::Partition;
    ///
    /// let record = AgentRecord::new("finance", AgentDefinition::new("Finance", "home"),
    ///     AgentRecordState::Active, AgentOrigin::ByHand, SubjectRef::human("alice"), chrono::Utc::now());
    /// let principal = record.principal("adk-rust", &Partition::new("home", "home"));
    /// assert_eq!(principal.id.as_str(), "urn:adk:agent:adk-rust/finance");
    /// assert_eq!(principal.class.as_deref(), Some("home"));
    /// assert_eq!(principal.sponsor, Some(SubjectRef::human("alice")));
    /// ```
    pub fn principal(&self, app_name: &str, area: &Partition) -> AgentPrincipal {
        let sponsor = self.approved_by.clone().unwrap_or_else(|| self.created_by.clone());
        area.principal(app_name, &self.id).with_sponsor(sponsor)
    }

    /// The agent's id as an [`AgentId`]: `urn:adk:agent:{app}/{id}`.
    pub fn agent_id(&self, app_name: &str) -> AgentId {
        AgentPrincipal::derived(app_name, &self.id).id
    }
}

/// Checks `id` against the id rules every added agent keeps (spec 3.3 §5.7): the pattern
/// `[a-z0-9][a-z0-9-]{0,39}` and no reserved id ([`RESERVED_AGENT_IDS`]); `@` and `.` never
/// match the pattern. The host's own targets are the agent table's to check.
///
/// # Errors
///
/// Returns a `kernel.agent.invalid` error.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::check_agent_id;
///
/// assert!(check_agent_id("finance-2").is_ok());
/// assert!(check_agent_id("adk-rust").is_err());
/// assert!(check_agent_id("adk-rust@work").is_err());
/// assert!(check_agent_id("-finance").is_err());
/// ```
pub fn check_agent_id(id: &str) -> Result<()> {
    let mut chars = id.chars();
    let pattern = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && id.len() <= MAX_AGENT_ID_LEN;
    if !pattern {
        return Err(invalid(format!(
            "`{id}` is not an agent id: lowercase letters, digits and `-`, starting with a \
             letter or digit, at most {MAX_AGENT_ID_LEN} characters"
        )));
    }
    if RESERVED_AGENT_IDS.contains(&id) {
        return Err(invalid(format!("`{id}` is reserved for the kernel's own agents")));
    }
    Ok(())
}

/// The id an agent named `display_name` gets (spec 3.3 §5.7, S-I14): its name in lowercase
/// ASCII letters and digits joined by `-`, at most [`MAX_AGENT_ID_LEN`] characters, or
/// [`FALLBACK_AGENT_ID`] when it has no letter or digit; then `-2`, `-3`, … while `taken`
/// says the id was ever used or is reserved.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::agent_id_for;
///
/// assert_eq!(agent_id_for("Finance", |_| false), "finance");
/// assert_eq!(agent_id_for("Finance", |id| id == "finance"), "finance-2");
/// assert_eq!(agent_id_for("Bills & Taxes!", |_| false), "bills-taxes");
/// assert_eq!(agent_id_for("日本", |_| false), "agent");
/// assert_eq!(agent_id_for("…", |id| id == "agent"), "agent-2");
/// ```
pub fn agent_id_for(display_name: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut base = String::new();
    for c in display_name.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if c.is_ascii() && !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let mut base = base.trim_end_matches('-').to_string();
    if base.is_empty() {
        base = FALLBACK_AGENT_ID.to_string();
    }
    let fits = |base: &str, suffix: &str| {
        let room = MAX_AGENT_ID_LEN - suffix.len();
        let cut = &base[..base.len().min(room)];
        format!("{}{suffix}", cut.trim_end_matches('-'))
    };
    let usable = |id: &str| !taken(id) && check_agent_id(id).is_ok();
    let first = fits(&base, "");
    if usable(&first) {
        return first;
    }
    let mut n: u64 = 2;
    loop {
        let candidate = fits(&base, &format!("-{n}"));
        if usable(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> AgentRecord {
        AgentRecord::new(
            id,
            AgentDefinition::new("Finance", "default"),
            AgentRecordState::Active,
            AgentOrigin::ByHand,
            SubjectRef::human("alice"),
            Utc::now(),
        )
    }

    #[test]
    fn ids_follow_the_pattern_and_skip_reserved_ones() {
        for id in ["finance", "a", "agent-2", "0day", &"a".repeat(40)] {
            assert!(check_agent_id(id).is_ok(), "{id}");
        }
        for id in [
            "",
            "Finance",
            "-finance",
            "fin.ance",
            "harness.goal",
            "adk-rust@work",
            "kernel.wake",
            "adk-rust",
            "self-improver",
            "finance_2",
            &"a".repeat(41),
        ] {
            assert!(check_agent_id(id).is_err(), "{id}");
        }
    }

    #[test]
    fn a_removed_agents_id_is_never_given_again() {
        let mut ever = vec!["finance".to_string()];
        let second = agent_id_for("Finance", |id| ever.iter().any(|used| used == id));
        assert_eq!(second, "finance-2");
        ever.push(second);
        assert_eq!(agent_id_for("FINANCE", |id| ever.iter().any(|used| used == id)), "finance-3");
        // A name that would be reserved moves on to a free id.
        assert_eq!(agent_id_for("ADK-Rust", |_| false), "adk-rust-2");
        assert_eq!(agent_id_for("Self improver", |_| false), "self-improver-2");
    }

    #[test]
    fn long_names_keep_ids_within_the_limit() {
        let name = "A very long agent name that goes on and on past forty characters";
        let first = agent_id_for(name, |_| false);
        assert!(first.len() <= MAX_AGENT_ID_LEN && !first.ends_with('-'), "{first}");
        let second = agent_id_for(name, |id| id == first);
        assert!(second.len() <= MAX_AGENT_ID_LEN && second.ends_with("-2"), "{second}");
        assert!(check_agent_id(&second).is_ok());
    }

    #[test]
    fn names_and_folders_are_checked() {
        let base = AgentDefinition::new("Finance", "default");
        assert!(base.clone().validate().is_ok());
        for bad in ["", "   ", "You", "you", &"x".repeat(41), "Two\nlines"] {
            let mut definition = base.clone();
            definition.display_name = bad.to_string();
            assert!(definition.validate().is_err(), "{bad:?}");
        }
        for bad in ["../Mail", "Invoices/../../etc", "/etc", "\\\\server", "C:\\Users", "~/x"] {
            let definition = base
                .clone()
                .with_uses(AgentUses::default().with_files(FileAccess::Read).with_folder(bad));
            assert!(definition.validate().is_err(), "{bad}");
        }
        let nested = base
            .clone()
            .with_uses(AgentUses::default().with_folder("./Invoices//2026/"))
            .normalized();
        assert_eq!(nested.uses.folder.as_deref(), Some("Invoices/2026"));
        let long = base.clone().with_instructions("x".repeat(MAX_AGENT_INSTRUCTIONS_LEN + 1));
        assert!(long.validate().is_err());
        assert!(base.clone().with_daily_limit(0).validate().is_err());
        let hosts = base
            .clone()
            .with_uses(AgentUses::default().with_web(WebAccess::listed(["https://x.io/a"])));
        assert!(hosts.validate().is_err());
    }

    #[test]
    fn the_digest_covers_the_definition_only() {
        let first = record("finance");
        let mut later = first.clone().with_state(AgentRecordState::Paused);
        later.version = 7;
        later.updated_at = first.updated_at + chrono::Duration::hours(1);
        later.origin = AgentOrigin::Suggested {
            agent: AgentId::new("urn:adk:agent:adk-rust/adk-rust").unwrap(),
            session: "s".into(),
            run: None,
            unattended: true,
        };
        assert_eq!(first.digest, later.digest);
        let renamed = first.clone().with_definition(first.definition().with_description("Bills"));
        assert_ne!(first.digest, renamed.digest);
        assert!(renamed.validate().is_ok());
        // Order and spacing of sets do not change it.
        let a = AgentDefinition::new(" Finance ", "default")
            .with_uses(AgentUses::default().with_pack("b").with_pack("a").with_pack("a"));
        let b = AgentDefinition::new("Finance", "default")
            .with_color(a.color)
            .with_uses(AgentUses::default().with_pack("a").with_pack("b"));
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn a_change_never_moves_the_area() {
        let record = record("finance");
        let moved = record.clone().with_definition(AgentDefinition::new("Finance", "work"));
        assert_eq!(moved.area.as_str(), "default");
        assert!(moved.validate().is_ok());
    }

    #[test]
    fn the_wire_form_refuses_unknown_fields() {
        let mut wire = serde_json::to_value(record("finance")).unwrap();
        wire["apiKey"] = serde_json::json!("sk-123");
        assert!(serde_json::from_value::<AgentRecord>(wire).is_err());
    }

    #[test]
    fn colors_match_the_surface_palette_hash() {
        // FNV-1a of "ADK-Rust" over eight colours, as adk-surface's avatar picks it.
        let hash = "ADK-Rust".bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        assert_eq!(AvatarColor::for_name("ADK-Rust").index(), (hash % 8) as usize);
        for (index, color) in AvatarColor::ALL.into_iter().enumerate() {
            assert_eq!(color.index(), index);
            assert_eq!(color.as_str().parse::<AvatarColor>().unwrap(), color);
        }
    }

    #[test]
    fn uses_over_a_ceiling_are_named() {
        let ceiling = AgentUses::default()
            .with_files(FileAccess::Read)
            .with_web(WebAccess::listed(["example.com"]))
            .with_pack("search")
            .with_driver("calendar");
        let within = AgentUses::default()
            .with_files(FileAccess::Read)
            .with_web(WebAccess::listed(["example.com"]))
            .with_driver("calendar");
        assert!(within.over(&ceiling).is_empty());
        let over = AgentUses::default()
            .with_files(FileAccess::ReadWrite)
            .with_shell(ShellAccess::Host)
            .with_web(WebAccess::listed(["example.com", "evil.test"]))
            .with_pack("payments")
            .with_driver("mail");
        assert_eq!(
            over.over(&ceiling),
            [
                "changing files",
                "a shell on this computer",
                "the sites evil.test",
                "the payments pack",
                "the mail driver"
            ]
        );
        assert_eq!(AgentUses::default().with_web(WebAccess::Any).over(&ceiling), ["any site"]);
    }
}
