//! Driver classes: the tools each class of driver offers, their effect classes, their
//! partitions and their default autonomy caps, as kernel driver manifests (spec 3.5 §7.13,
//! §7.10; spec 3.2 §7.7).

use std::collections::BTreeSet;
use std::fmt;

use adk_kernel::KernelError;
use adk_kernel::drivers::{DriverManifest, EffectClass};
use adk_kernel::partition::PartitionId;
use adk_tool::mcp::manager::McpServerConfig;
use awp_types::AutonomyLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The partition of a website's public agents (spec 3.5 §7.10): Concierge, Agent desk,
/// Inbox.
pub const PUBLIC_PARTITION: &str = "public";

/// The partition of a website's back office (spec 3.5 §7.10): Steward, Growth, Revenue,
/// Guardian, Finance.
pub const BACK_OFFICE_PARTITION: &str = "back-office";

/// Which of a website's two partitions a driver tool serves (spec 3.5 §7.10).
///
/// Untrusted text — an inbound email, a mention — is read in the public partition only;
/// the back office never reads it raw (A-I3), so a tool that reads such text is
/// [`Public`](Self::Public) and every tool that publishes, changes DNS, spends or sends in
/// bulk is [`BackOffice`](Self::BackOffice) (A-I2).
///
/// # Example
///
/// ```rust
/// use adk_drivers::Side;
///
/// assert_eq!(Side::Public.partition().as_str(), "public");
/// assert_eq!(Side::BackOffice.partition().as_str(), "back-office");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Side {
    /// The public partition: reads untrusted input and replies in its thread.
    Public,
    /// The back-office partition: everything else.
    BackOffice,
}

impl Side {
    /// The partition this side runs in by default.
    pub fn partition(self) -> PartitionId {
        PartitionId::new(match self {
            Self::Public => PUBLIC_PARTITION,
            Self::BackOffice => BACK_OFFICE_PARTITION,
        })
    }

    /// The side's name: `public` or `backOffice`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::BackOffice => "backOffice",
        }
    }
}

/// How far a job goes with a tool before a person decides, by default (spec 3.5 §7.13,
/// "Default autonomy cap"; §7.11 autonomy ladder).
///
/// The cap is the level a job using the tool starts at and is offered promotion from;
/// promotion is an owner's decision (A-I13). [`AlwaysHeld`](Self::AlwaysHeld) is an
/// invariant, not a level: no mode, standing rule or promotion lifts it (A-I21), and
/// [`DriverCapPolicy`](crate::DriverCapPolicy) enforces it on every call.
///
/// # Example
///
/// ```rust
/// use adk_drivers::AutonomyCap;
///
/// assert!(AutonomyCap::AlwaysHeld.is_invariant());
/// assert!(!AutonomyCap::Propose.is_invariant());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AutonomyCap {
    /// Reads only.
    Read,
    /// The job proposes; a person decides each call.
    Propose,
    /// Within the job's playbook and the recipient's consent.
    WithinPlaybook,
    /// Within the owner's budget; a call beyond it is held.
    WithinBudget,
    /// Held for a person in every mode (A-I21).
    AlwaysHeld,
}

impl AutonomyCap {
    /// Whether the cap holds in every mode, whatever the job's level.
    pub fn is_invariant(self) -> bool {
        matches!(self, Self::AlwaysHeld)
    }

    /// The autonomy level a job using the tool starts at (spec 3.5 §7.11, §7.20): Observe
    /// for reads, Propose, "act within budget" for the playbook and budget caps, and "act
    /// with approval" for a held tool — whose calls [`DriverCapPolicy`](crate::DriverCapPolicy)
    /// holds on every rung.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_drivers::AutonomyCap;
    /// use awp_types::AutonomyLevel;
    ///
    /// assert_eq!(AutonomyCap::Propose.starting_level(), AutonomyLevel::Propose);
    /// assert_eq!(AutonomyCap::AlwaysHeld.starting_level(), AutonomyLevel::ActWithApproval);
    /// ```
    pub fn starting_level(self) -> AutonomyLevel {
        match self {
            Self::Read => AutonomyLevel::Observe,
            Self::Propose => AutonomyLevel::Propose,
            Self::WithinPlaybook | Self::WithinBudget => AutonomyLevel::ActWithinBudget,
            Self::AlwaysHeld => AutonomyLevel::ActWithApproval,
        }
    }
}

/// One tool of a driver class (spec 3.5 §7.13): its name, effect class, side and default
/// autonomy cap, and the JSON Schema of its arguments.
///
/// The same name and schema serve an in-process adapter's toolset and any MCP server that
/// satisfies the class's manifest, so an agent's playbook does not depend on which one
/// the owner connected.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ToolSpec {
    /// The tool's name, prefixed by its class.
    pub name: &'static str,
    /// What the tool does, as the model reads it.
    pub description: &'static str,
    /// The effect class its manifest declares (spec 3.2 §7.7).
    pub class: EffectClass,
    /// A further effect the call has, such as a campaign launch that both spends and
    /// publishes; the manifest declares [`class`](Self::class), the stricter.
    pub also: Option<EffectClass>,
    /// The partition side it serves.
    pub side: Side,
    /// Its default autonomy cap.
    pub cap: AutonomyCap,
    /// The JSON pointer of the amount argument a [`AutonomyCap::WithinBudget`] call is
    /// measured by, in minor units; `None` for a call that never adds spend.
    pub budget_arg: Option<&'static str>,
    /// The JSON Schema of its arguments.
    pub schema: fn() -> Value,
}

impl ToolSpec {
    /// The names of the arguments its schema requires.
    pub fn required_args(&self) -> Vec<String> {
        required_of(&(self.schema)())
    }
}

fn required_of(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|required| required.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// A class of driver (spec 3.5 §7.13).
///
/// # Example
///
/// ```rust
/// use adk_drivers::DriverKind;
///
/// assert_eq!(DriverKind::Dns.as_str(), "dns");
/// assert_eq!(DriverKind::ALL.len(), 7);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DriverKind {
    /// DNS records.
    Dns,
    /// Domain registration: expiry and renewal.
    Registrar,
    /// Email: inbox, replies, sequences and newsletters, delivery statistics.
    Email,
    /// Site analytics.
    Analytics,
    /// Search performance.
    SearchConsole,
    /// Social: mentions, messages, replies and posts.
    Social,
    /// Advertising: spend and performance, campaigns, bids and pauses.
    Advertising,
}

impl DriverKind {
    /// Every class, in the order of spec 3.5 §7.13's table.
    pub const ALL: [DriverKind; 7] = [
        Self::Dns,
        Self::Registrar,
        Self::Email,
        Self::Analytics,
        Self::SearchConsole,
        Self::Social,
        Self::Advertising,
    ];

    /// The class's name, which prefixes its tools.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Registrar => "registrar",
            Self::Email => "email",
            Self::Analytics => "analytics",
            Self::SearchConsole => "search_console",
            Self::Social => "social",
            Self::Advertising => "ads",
        }
    }

    /// The class's tools.
    pub fn class(self) -> DriverClass {
        let tools: &'static [ToolSpec] = match self {
            Self::Dns => crate::dns::DNS_TOOLS,
            Self::Registrar => crate::dns::REGISTRAR_TOOLS,
            Self::Email => crate::email::EMAIL_TOOLS,
            Self::Analytics => crate::analytics::ANALYTICS_TOOLS,
            Self::SearchConsole => crate::analytics::SEARCH_CONSOLE_TOOLS,
            Self::Social => crate::social::SOCIAL_TOOLS,
            Self::Advertising => crate::ads::ADS_TOOLS,
        };
        DriverClass { kind: self, tools }
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The spec of every driver tool of every class.
pub fn all_tools() -> impl Iterator<Item = &'static ToolSpec> {
    DriverKind::ALL.into_iter().flat_map(|kind| kind.class().tools.iter())
}

/// The spec of the driver tool named `name`, of any class.
pub fn tool_spec(name: &str) -> Option<&'static ToolSpec> {
    all_tools().find(|spec| spec.name == name)
}

/// A class of driver and its tools (spec 3.5 §7.13), as kernel driver manifests.
///
/// A driver is a kernel driver — a supervised MCP server under a manifest the owner
/// approved — or an in-process adapter with the same tools. A class's manifest lists its
/// tools with their effect classes for one side; [`check_manifest`](Self::check_manifest)
/// and [`check_listing`](Self::check_listing) tell whether any MCP server and its manifest
/// satisfy the class, so the owner may connect whichever server they choose (D-13).
///
/// # Example
///
/// ```rust
/// use adk_drivers::{DriverKind, Side};
/// use adk_kernel::drivers::{DriverManifest, EffectClass};
///
/// let dns = DriverKind::Dns.class();
/// let text = dns.manifest_toml("dns", "mcp-dns", &[], Side::BackOffice);
/// let manifest = DriverManifest::from_toml(&text).unwrap();
/// assert_eq!(manifest.class_of("dns_change_records"), EffectClass::PublishPublic);
/// assert!(dns.check_manifest(&manifest, Side::BackOffice).is_satisfied());
/// ```
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DriverClass {
    /// Which class.
    pub kind: DriverKind,
    /// Its tools, on both sides.
    pub tools: &'static [ToolSpec],
}

impl DriverClass {
    /// The tool named `name`.
    pub fn tool(&self, name: &str) -> Option<&'static ToolSpec> {
        self.tools.iter().find(|spec| spec.name == name)
    }

    /// The tools that serve `side`.
    pub fn tools_for(&self, side: Side) -> impl Iterator<Item = &'static ToolSpec> + use<> {
        self.tools.iter().filter(move |spec| spec.side == side)
    }

    /// Whether the class has a tool on `side`.
    pub fn serves(&self, side: Side) -> bool {
        self.tools_for(side).next().is_some()
    }

    /// The kernel manifest of driver `id` running `server` for `side`: the side's tools with
    /// their effect classes, serving the side's partition.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when `id` or the server is not a valid driver (see
    /// [`DriverManifest::new`]), or the class has no tool on `side`.
    pub fn manifest(
        &self,
        id: &str,
        server: McpServerConfig,
        side: Side,
    ) -> std::result::Result<DriverManifest, KernelError> {
        if !self.serves(side) {
            return Err(KernelError::Config {
                reason: format!("the {} driver class has no {} tool", self.kind, side.as_str()),
            });
        }
        let mut manifest = DriverManifest::new(id, server)?.with_partitions([side.partition()]);
        for spec in self.tools_for(side) {
            manifest = manifest.with_tool(spec.name, spec.class);
        }
        Ok(manifest)
    }

    /// The TOML manifest of driver `id` running `command` with `args` for `side`, as the
    /// owner reviews and mounts it (`adk-rust drivers mount <file>`).
    pub fn manifest_toml(&self, id: &str, command: &str, args: &[&str], side: Side) -> String {
        let quoted = |text: &str| serde_json::to_string(text).unwrap_or_default();
        let mut out = format!(
            "# The {} driver's {} tools (spec 3.5 §7.13).\nid = {}\ncommand = {}\n",
            self.kind,
            side.as_str(),
            quoted(id),
            quoted(command)
        );
        if !args.is_empty() {
            let args: Vec<String> = args.iter().map(|arg| quoted(arg)).collect();
            out.push_str(&format!("args = [{}]\n", args.join(", ")));
        }
        out.push_str(&format!("partitions = [{}]\n\n[tools]\n", quoted(side.partition().as_str())));
        for spec in self.tools_for(side) {
            out.push_str(&format!("{} = {}\n", spec.name, quoted(spec.class.as_str())));
        }
        out
    }

    /// What keeps `manifest` from satisfying the class on `side`: a tool it does not list,
    /// a tool it classes less strictly than the contract, a tool of the other side, or a
    /// partition other than the side's.
    pub fn check_manifest(&self, manifest: &DriverManifest, side: Side) -> ManifestCheck {
        let mut gaps = Vec::new();
        let partition = side.partition();
        for listed in &manifest.partitions {
            if *listed != partition {
                gaps.push(Gap::WrongPartition { partition: listed.to_string() });
            }
        }
        for spec in self.tools_for(side) {
            match manifest.tools.get(spec.name) {
                None => gaps.push(Gap::MissingTool { tool: spec.name.to_string() }),
                Some(class) if !at_least_as_strict(*class, spec.class) => {
                    gaps.push(Gap::WeakerClass {
                        tool: spec.name.to_string(),
                        contract: spec.class,
                        manifest: *class,
                    });
                }
                Some(_) => {}
            }
        }
        for tool in manifest.tools.keys() {
            if self.tool(tool).is_some_and(|spec| spec.side != side) {
                gaps.push(Gap::ForeignTool { tool: tool.clone() });
            }
        }
        ManifestCheck { gaps }
    }

    /// What keeps an MCP server's tool listing from satisfying the class on `side`: a tool
    /// it does not offer, an argument the contract requires that its schema lacks, or a
    /// tool beyond the side's contract — which the kernel would serve to the partition as
    /// `other`, so the server must not offer it there.
    ///
    /// `listed` holds each tool's name and input schema, as the server lists them.
    pub fn check_listing(&self, side: Side, listed: &[(String, Value)]) -> ManifestCheck {
        let mut gaps = Vec::new();
        for spec in self.tools_for(side) {
            let Some((_, schema)) = listed.iter().find(|(name, _)| name == spec.name) else {
                gaps.push(Gap::MissingTool { tool: spec.name.to_string() });
                continue;
            };
            let offered: BTreeSet<&str> = schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| properties.keys().map(String::as_str).collect())
                .unwrap_or_default();
            for argument in spec.required_args() {
                if !offered.contains(argument.as_str()) {
                    gaps.push(Gap::MissingArgument { tool: spec.name.to_string(), argument });
                }
            }
        }
        for (name, _) in listed {
            if !self.tools_for(side).any(|spec| spec.name == name) {
                gaps.push(Gap::ForeignTool { tool: name.clone() });
            }
        }
        ManifestCheck { gaps }
    }
}

/// Whether a manifest's `declared` class is the contract's or stricter than it.
fn at_least_as_strict(declared: EffectClass, contract: EffectClass) -> bool {
    declared == contract || declared.strictness() > contract.strictness()
}

/// What keeps a manifest or a server's listing from satisfying a driver class (spec 3.5
/// §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum Gap {
    /// A tool of the contract is not there.
    MissingTool {
        /// The tool.
        tool: String,
    },
    /// The manifest classes a tool less strictly than the contract.
    WeakerClass {
        /// The tool.
        tool: String,
        /// The contract's class.
        contract: EffectClass,
        /// The manifest's class.
        manifest: EffectClass,
    },
    /// A tool that does not belong on this side.
    ForeignTool {
        /// The tool.
        tool: String,
    },
    /// The manifest serves a partition other than the side's.
    WrongPartition {
        /// The partition.
        partition: String,
    },
    /// The server's schema for a tool lacks an argument the contract requires.
    MissingArgument {
        /// The tool.
        tool: String,
        /// The argument.
        argument: String,
    },
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTool { tool } => write!(f, "`{tool}` is missing"),
            Self::WeakerClass { tool, contract, manifest } => {
                write!(f, "`{tool}` is classed `{manifest}`; the contract needs `{contract}`")
            }
            Self::ForeignTool { tool } => write!(f, "`{tool}` does not belong on this side"),
            Self::WrongPartition { partition } => {
                write!(f, "the manifest serves partition `{partition}`")
            }
            Self::MissingArgument { tool, argument } => {
                write!(f, "`{tool}` takes no `{argument}` argument")
            }
        }
    }
}

/// The result of checking a manifest or a listing against a driver class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ManifestCheck {
    /// Every gap found.
    pub gaps: Vec<Gap>,
}

impl ManifestCheck {
    /// Whether nothing keeps it from satisfying the class.
    pub fn is_satisfied(&self) -> bool {
        self.gaps.is_empty()
    }
}

impl fmt::Display for ManifestCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.gaps.is_empty() {
            return f.write_str("satisfies the driver class");
        }
        let gaps: Vec<String> = self.gaps.iter().map(ToString::to_string).collect();
        f.write_str(&gaps.join("; "))
    }
}
